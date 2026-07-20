import SwiftUI
import AppKit

// MARK: - Media Library + ML playlist CRUD

extension SparkampModel {
    // MARK: Media Library

    /// Open (or create) the media library DB and load initial data.
    func openMediaLibrary() {
        guard let ctx = ctx else { return }
        if !mlIsOpen {
            sparkamp_ml_open(ctx)
            mlIsOpen = true
        }
        mlRefreshFolders()
        mlRefreshSavedPlaylists()
        mediaLibraryVisible = true
    }

    func mlRefreshFolders() {
        guard let ctx = ctx else { return }
        let count = Int(sparkamp_ml_folder_count(ctx))
        mlFolders = (0..<count).compactMap { i in
            guard let ptr = sparkamp_ml_folder_path(ctx, Int32(i)) else { return nil }
            defer { sparkamp_free_string(ptr) }
            return String(cString: ptr)
        }
    }

    func mlRefreshSavedPlaylists() {
        guard let ctx = ctx else { return }
        let count = Int(sparkamp_ml_playlist_count(ctx))
        mlSavedPlaylists = (0..<count).compactMap { i in
            guard let ptr = sparkamp_ml_playlist_name(ctx, Int32(i)) else { return nil }
            defer { sparkamp_free_string(ptr) }
            let dbId = sparkamp_ml_playlist_id(ctx, Int32(i))
            let path = mlPlaylistPath(id: dbId) ?? ""
            return MLPlaylistItem(id: dbId, name: String(cString: ptr), path: path)
        }
        // Any caller that mutates the saved-playlists list also affects what
        // the open editor might be showing; nudge content observers so the
        // editor reloads its track list (handles e.g. external rename → path
        // change → editor's cached tracks become stale).
        mlPlaylistContentTrigger &+= 1
    }

    /// One-shot fetch of every track in the library, ignoring the current
    /// search filter / sort / pagination state of `mlTracks`.  Returns a
    /// fresh array without mutating `mlTracks` so the Files-view filter
    /// state survives.  Used by callers that need to scan the whole library
    /// (e.g. the playlist editor's "Add Folder" picker).
    func mlAllTracks() -> [MLTrack] {
        guard let ctx = ctx else { return [] }
        let limit = 100_000
        let buf = UnsafeMutablePointer<SparkampLibTrack>.allocate(capacity: limit)
        defer { buf.deallocate() }
        let count = "".withCString { qPtr in
            sparkamp_ml_get_tracks(ctx, qPtr, nil, 0, 0, Int32(limit), buf)
        }
        return (0..<Int(count)).map { MLTrack(from: buf[$0]) }
    }

    /// Look up a single library track by absolute path, or nil if the
    /// path isn't in the library.  Implemented as an `mlAllTracks` scan
    /// for simplicity — fine for typical library sizes; if libraries grow
    /// large enough to make this a bottleneck, add a dedicated FFI lookup.
    func mlGetTrackByPath(_ path: String) -> MLTrack? {
        mlAllTracks().first(where: { $0.path == path })
    }

    /// Fetch tracks from the library, applying optional search query and sort.
    /// Loads up to `limit` rows starting at `offset`.
    func mlFetchTracks(
        query: String = "",
        sortCol: String? = nil,
        sortDesc: Bool = false,
        offset: Int = 0,
        limit: Int = 10_000
    ) {
        guard let ctx = ctx else { return }
        let buf = UnsafeMutablePointer<SparkampLibTrack>.allocate(capacity: limit)
        defer { buf.deallocate() }
        let count = query.withCString { qPtr -> Int32 in
            if let col = sortCol {
                return col.withCString { colPtr in
                    sparkamp_ml_get_tracks(ctx, qPtr, colPtr, sortDesc ? 1 : 0,
                                          Int32(offset), Int32(limit), buf)
                }
            } else {
                return sparkamp_ml_get_tracks(ctx, qPtr, nil, 0,
                                              Int32(offset), Int32(limit), buf)
            }
        }
        mlTracks = (0..<Int(count)).map { MLTrack(from: buf[$0]) }
    }

    func mlAddFolder(_ path: String) {
        guard let ctx = ctx else { return }
        path.withCString { sparkamp_ml_add_folder(ctx, $0, nil, nil, nil) }
        mlScanRunning = true
        mlScanDone = 0
        mlScanTotal = 0
        mlRefreshFolders()
        // Phase 1 (fast, synchronous) already ran inside sparkamp_ml_add_folder.
        // Reload immediately so filename-only rows appear before Phase 2 finishes.
        mlFetchTracks()
    }

    func mlRemoveFolder(_ path: String) {
        guard let ctx = ctx else { return }
        path.withCString { sparkamp_ml_remove_folder(ctx, $0) }
        mlRefreshFolders()
        mlFetchTracks()
    }

    func mlRescanAll() {
        guard let ctx = ctx else { return }
        sparkamp_ml_rescan_all(ctx, nil, nil, nil)
        mlScanRunning = true
        mlScanDone = 0
        mlScanTotal = 0
        // Show current state immediately; tick() will refresh periodically.
        mlFetchTracks()
    }

    func mlCancelScan() {
        guard let ctx = ctx else { return }
        sparkamp_ml_cancel_scan(ctx)
    }

    func mlAddToPlaylist(ids: [Int64]) {
        guard let ctx = ctx else { return }
        var idArray = ids
        idArray.withUnsafeMutableBufferPointer { buf in
            sparkamp_ml_add_tracks_to_playlist(ctx, buf.baseAddress, Int32(ids.count))
        }
        refreshPlaylist()
        saveState()
    }

    func mlSetCurrentPlaylist(_ index: Int) {
        guard let ctx = ctx else { return }
        sparkamp_ml_set_current_playlist(ctx, Int32(index))
        refreshAll()
        saveState()
    }

    func mlReplacePlaylistWith(ids: [Int64]) {
        guard let ctx = ctx else { return }
        clearPlaylist()
        mlAddToPlaylist(ids: ids)
        if sparkamp_get_autoplay_on_add(ctx) {
            sparkamp_playlist_jump(ctx, 0)
            sparkamp_play(ctx)
            refreshCurrentTrackInfo()
        }
    }

    /// Called when a track is double-clicked in the ML table.
    ///
    /// Respects both Settings preferences:
    /// - **Playlist add behavior** (Append / Replace) decides whether to clear
    ///   the playlist first or just append.
    /// - **Autoplay when files are added** decides whether the new track
    ///   starts playing.  When autoplay is off, double-click only adds
    ///   (matches GTK's behaviour and the explicit Add-to-Playlist button).
    ///
    /// Autoplay-on, append-mode: only auto-plays when the playlist was
    /// empty before the add.  This avoids interrupting the currently
    /// playing track when the user is queueing more music.
    func mlDoubleClickTracks(ids: [Int64]) {
        guard let ctx = ctx else { return }
        let shouldReplace = Int(sparkamp_get_playlist_add_behavior(ctx)) == 1
        let autoplay     = sparkamp_get_autoplay_on_add(ctx)
        let indexBefore  = Int(sparkamp_playlist_len(ctx))
        let wasEmpty     = indexBefore == 0
        if shouldReplace {
            clearPlaylist()
            mlAddToPlaylist(ids: ids)
            if autoplay {
                sparkamp_playlist_jump(ctx, 0)
                sparkamp_play(ctx)
            }
        } else {
            mlAddToPlaylist(ids: ids)
            // Don't interrupt a track the user is already listening to.
            if autoplay && wasEmpty {
                sparkamp_playlist_jump(ctx, Int32(indexBefore))
                sparkamp_play(ctx)
            }
        }
        refreshCurrentTrackInfo()
    }

    /// Load album artwork from a file path and open the artwork zoom window.
    func mlViewArtForPath(_ path: String) {
        let tagCtx = path.withCString { sparkamp_tag_open($0) }
        defer { sparkamp_tag_close(tagCtx) }
        var artLen: Int32 = 0
        if let dataPtr = sparkamp_tag_get_artwork_data(tagCtx, &artLen), artLen > 0 {
            let data = Data(bytes: dataPtr, count: Int(artLen))
            sparkamp_tag_free_artwork(dataPtr, artLen)
            if let image = NSImage(data: data) {
                // Static zoom, not the A6 follow-current-track mode — a
                // stale `true` here would let the next track change
                // overwrite this specific track's art moments later.
                artworkFollowsPlayback = false
                artworkImage = image
                artworkWindowVisible = true
                artworkWindowRequest &+= 1  // re-front if already open
            }
        }
    }

    func mlRemoveTracks(ids: [Int64]) {
        guard let ctx = ctx else { return }
        for id in ids {
            sparkamp_ml_remove_track(ctx, id)
        }
        mlFetchTracks()
    }

    /// Force the library DB to re-read tags + duration for `path`.  Call after
    /// the ID3 editor saves so the Files-view row picks up the new metadata
    /// without waiting for a full library rescan.  Bumps `mlReloadTrigger`
    /// so the open Media Library window re-fetches its current page.
    func mlRescanTrack(path: String) {
        guard let ctx = ctx else { return }
        path.withCString { sparkamp_ml_rescan_track(ctx, $0) }
        mlReloadTrigger &+= 1
    }

    /// Upsert a batch of file paths into the library DB without registering
    /// new watched folders.  Paths whose parent dir lives outside every
    /// watched folder are silently skipped.  Returns the count actually
    /// added.  Bumps `mlReloadTrigger` so the Files view picks up the new
    /// rows on its next render.
    @discardableResult
    func mlAddFilesToLibrary(paths: [String]) -> Int {
        guard let ctx = ctx, !paths.isEmpty else { return 0 }
        let nsStrings: [NSString]              = paths.map { $0 as NSString }
        let cPaths:    [UnsafePointer<CChar>?] = nsStrings.map { $0.utf8String }
        let added: Int32 = cPaths.withUnsafeBufferPointer { buf in
            let mutablePtr = UnsafeMutablePointer<UnsafePointer<CChar>?>(
                mutating: buf.baseAddress)
            return sparkamp_ml_add_files(ctx, mutablePtr, Int32(paths.count))
        }
        if added > 0 { mlReloadTrigger &+= 1 }
        return Int(added)
    }

    func mlOpenTagEditorForPath(_ path: String) {
        id3TrackIndex = -1          // direct-path mode; drop any stale playlist index
        id3DirectPath = path
        id3EditorVisible = true
        id3Request &+= 1
    }

    // MARK: ML Playlist CRUD

    /// Fetch all tracks in a saved playlist by its row ID.
    func mlGetPlaylistTracks(id: Int64) -> [MLTrack] {
        guard let ctx = ctx else { return [] }
        let limit = 10_000
        let buf = UnsafeMutablePointer<SparkampLibTrack>.allocate(capacity: limit)
        defer { buf.deallocate() }
        let count = sparkamp_ml_get_playlist_tracks(ctx, id, buf, Int32(limit))
        return (0..<Int(count)).map { MLTrack(from: buf[$0]) }
    }

    /// Create a new empty playlist.  Returns the new playlist's row ID, or -1 on failure.
    func mlCreatePlaylist(name: String) -> Int64 {
        guard let ctx = ctx else { return -1 }
        let id = name.withCString { sparkamp_ml_create_playlist(ctx, $0) }
        if id >= 0 { mlRefreshSavedPlaylists() }
        return id
    }

    /// Delete a playlist by row ID (DB only; playlist file is kept on disk).
    func mlDeletePlaylist(id: Int64) {
        guard let ctx = ctx else { return }
        sparkamp_ml_delete_playlist(ctx, id)
        mlRefreshSavedPlaylists()
    }

    /// Rename a playlist by row ID.
    func mlRenamePlaylist(id: Int64, name: String) {
        guard let ctx = ctx else { return }
        name.withCString { sparkamp_ml_rename_playlist(ctx, id, $0) }
        mlRefreshSavedPlaylists()
    }

    /// Overwrite a saved playlist's file (.m3u8 or legacy .m3u) with the
    /// given ordered track IDs.
    func mlSavePlaylist(id: Int64, trackIds: [Int64]) {
        guard let ctx = ctx else { return }
        var ids = trackIds
        ids.withUnsafeMutableBufferPointer { buf in
            sparkamp_ml_save_playlist(ctx, id, buf.baseAddress, Int32(trackIds.count))
        }
        // The on-disk playlist file has new contents — let any open editor reload.
        mlPlaylistContentTrigger &+= 1
    }

    /// Active-playlist track path at `index`, or nil if out of range.
    /// Used by the active-playlist Add-to-Playlist menu to feed raw paths
    /// into `mlAppendPathsToPlaylist`.
    func playlistTrackPath(index: Int) -> String? {
        guard let ctx = ctx else { return nil }
        guard let cstr = sparkamp_playlist_get_path(ctx, Int32(index)) else { return nil }
        defer { sparkamp_free_string(cstr) }
        return String(cString: cstr)
    }

    /// Append raw track paths to a saved playlist's file on disk (.m3u8 or
    /// legacy .m3u).  The core emits an `#EXTINF` line for every entry,
    /// looking up duration / artist / title from the library where known.
    func mlAppendPathsToPlaylist(playlistId: Int64, paths: [String]) {
        guard let ctx = ctx, !paths.isEmpty else { return }
        let nsStrings: [NSString]              = paths.map { $0 as NSString }
        let cPaths:    [UnsafePointer<CChar>?] = nsStrings.map { $0.utf8String }
        cPaths.withUnsafeBufferPointer { buf in
            let mutablePtr = UnsafeMutablePointer<UnsafePointer<CChar>?>(
                mutating: buf.baseAddress)
            sparkamp_ml_append_paths_to_playlist(ctx, playlistId,
                                                 mutablePtr, Int32(paths.count))
        }
        // Notify any open editor of that playlist so it re-reads the file.
        mlPlaylistContentTrigger &+= 1
    }

    /// Default destination directory for newly-created saved playlists
    /// (Save As… and the active-playlist "New Playlist" context-menu action).
    ///
    /// 1. First watched folder in the media library, if one exists on disk.
    /// 2. The current user's `~/Music` folder (created implicitly by macOS).
    /// 3. The Sparkamp-managed playlists directory as a last-resort fallback.
    ///
    /// Routing through this avoids the legacy create-in-managed-dir path,
    /// which had the side effect of registering Sparkamp's internal
    /// playlists folder as a "watched folder" via `add_playlist_file`.
    func mlDefaultSaveAsDir() -> URL {
        if let first = mlFolders.first {
            let url = URL(fileURLWithPath: first, isDirectory: true)
            if FileManager.default.fileExists(atPath: url.path) { return url }
        }
        let music = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Music", isDirectory: true)
        return music
    }

    /// Create a new playlist with `name` containing `trackPaths`.
    ///
    /// When `directory` is nil the file is written to Sparkamp's managed
    /// playlists folder (as `<name>.m3u8`) via the existing FFI helper.
    /// When `directory` is provided (typical Save-As flow with NSSavePanel)
    /// the file is written at `<directory>/<name>.m3u8` by the Rust core
    /// so it gets `#EXTINF` lines and is registered in the library in one
    /// step.  Returns the new playlist row id, or -1 on failure.
    func mlSavePlaylistAs(name: String,
                          trackPaths: [String],
                          directory: URL? = nil) -> Int64 {
        guard let ctx = ctx else { return -1 }
        let nsStrings: [NSString]              = trackPaths.map { $0 as NSString }
        let cPaths:    [UnsafePointer<CChar>?] = nsStrings.map { $0.utf8String }
        if let dir = directory {
            // Custom location — let the core write the file (so EXTINF
            // metadata is emitted) and register it atomically.
            let dest = dir.appendingPathComponent("\(name).m3u8")
            return cPaths.withUnsafeBufferPointer { buf in
                dest.path.withCString { destCStr in
                    let mutablePtr = UnsafeMutablePointer<UnsafePointer<CChar>?>(
                        mutating: buf.baseAddress)
                    return sparkamp_ml_save_playlist_to_path(ctx, destCStr,
                                                             mutablePtr,
                                                             Int32(trackPaths.count))
                }
            }
        }
        // Default: managed-directory create-and-write via existing FFI.
        return cPaths.withUnsafeBufferPointer { buf in
            name.withCString { nameCStr in
                let mutablePtr = UnsafeMutablePointer<UnsafePointer<CChar>?>(
                    mutating: buf.baseAddress)
                return sparkamp_ml_save_playlist_as(ctx, nameCStr,
                                                    mutablePtr,
                                                    Int32(trackPaths.count))
            }
        }
    }

    /// Return true if the playlist lives in Sparkamp's managed playlists directory.
    func mlPlaylistIsManaged(id: Int64) -> Bool {
        guard let ctx = ctx else { return false }
        return sparkamp_ml_playlist_is_managed(ctx, id) != 0
    }

    /// Return the file path of the playlist, or nil on error.
    func mlPlaylistPath(id: Int64) -> String? {
        guard let ctx = ctx else { return nil }
        guard let ptr = sparkamp_ml_playlist_path(ctx, id) else { return nil }
        defer { sparkamp_free_string(ptr) }
        return String(cString: ptr)
    }

    func mlOpenAddFolderPicker() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.prompt = "Add to Library"
        panel.begin { [weak self] resp in
            guard resp == .OK, let self, let url = panel.url else { return }
            Task { @MainActor in self.mlAddFolder(url.path) }
        }
    }

}
