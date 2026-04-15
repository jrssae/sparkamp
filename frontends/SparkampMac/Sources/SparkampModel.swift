import Foundation
import AppKit

// MARK: - C-array string helper

/// Convert a fixed-size C byte array (imported as a tuple in Swift) to a String.
/// Stops at the first null byte; interprets as UTF-8.
func cBytesToString<T>(_ value: inout T) -> String {
    withUnsafeBytes(of: &value) { bytes in
        let end = bytes.firstIndex(of: 0) ?? bytes.endIndex
        return String(bytes: bytes[..<end], encoding: .utf8) ?? ""
    }
}

// MARK: - Media Library types

/// A single track row from the media library.
struct MLTrack: Identifiable {
    let id: Int64
    let path: String
    let title: String
    let artist: String
    let album: String
    let genre: String
    let year: Int
    let trackNum: Int
    let lengthSecs: Double
    let bitrate: Int
    let playCount: Int
    let scanned: Bool
    // Extended DB fields
    let albumArtist: String
    let discNum: Int
    let bpm: String
    let comment: String
    let composer: String
    let readOnly: Bool
    let hasArt: Bool
    let fileMissing: Bool

    var durationString: String { formatDuration(lengthSecs) }
    var filename: String { URL(fileURLWithPath: path).lastPathComponent }

    init(from c: SparkampLibTrack) {
        var c = c
        id          = c.id
        path        = cBytesToString(&c.path)
        title       = cBytesToString(&c.title)
        artist      = cBytesToString(&c.artist)
        album       = cBytesToString(&c.album)
        genre       = cBytesToString(&c.genre)
        year        = Int(c.year)
        trackNum    = Int(c.track_num)
        lengthSecs  = c.length_secs
        bitrate     = Int(c.bitrate)
        playCount   = Int(c.play_count)
        scanned     = c.scanned != 0
        albumArtist = cBytesToString(&c.album_artist)
        discNum     = Int(c.disc_num)
        bpm         = cBytesToString(&c.bpm)
        comment     = cBytesToString(&c.comment)
        composer    = cBytesToString(&c.composer)
        readOnly    = c.read_only != 0
        hasArt      = c.has_art != 0
        fileMissing = c.file_missing != 0
    }
}

// MARK: - Media Library playlist item

struct MLPlaylistItem: Identifiable {
    let id: Int64   // DB row id — stable key for CRUD operations
    let name: String
}

// MARK: - Dedup types

struct DedupTrackItem: Identifiable {
    let id: String   // path used as stable ID
    let path: String
    let title: String
    let artist: String
    let durationSecs: Double

    var durationString: String { formatDuration(durationSecs) }
    var filename: String { URL(fileURLWithPath: path).lastPathComponent }
}

struct DedupGroupItem: Identifiable {
    let id: UUID
    let confidence: Int   // 0 = Probable, 1 = Less Likely
    let tracks: [DedupTrackItem]

    var confidenceLabel: String { confidence == 0 ? "Probable" : "Less Likely" }
    var label: String {
        guard let first = tracks.first else { return "Unknown" }
        return first.artist.isEmpty ? first.title : "\(first.artist) — \(first.title)"
    }
}

// MARK: - Data types

struct PlaylistItem: Identifiable {
    let id: Int          // the playlist index
    let title: String
    let artist: String
    let albumArtist: String
    let duration: Double // seconds, -1 = unknown
    let broken: Bool
    let readOnly: Bool
    let fileMissing: Bool

    var durationString: String { formatDuration(duration) }

    /// Single-line display string: "Artist — Title" with album_artist fallback.
    var displayName: String { trackDisplayName(title: title, artist: artist, albumArtist: albumArtist) }
}

/// Shared display-name logic used by both the playlist and the marquee.
/// Returns "Artist — Title", falling back to albumArtist when artist is empty,
/// or just the title (which may be the filename stem) when neither is available.
func trackDisplayName(title: String, artist: String, albumArtist: String) -> String {
    let t = title.isEmpty ? "Unknown" : title
    if !artist.isEmpty      { return "\(artist) — \(t)" }
    if !albumArtist.isEmpty { return "\(albumArtist) — \(t)" }
    return t
}

func formatDuration(_ secs: Double) -> String {
    guard secs >= 0 else { return "--:--" }
    let total = Int(secs)
    let m = total / 60
    let s = total % 60
    return String(format: "%d:%02d", m, s)
}

// MARK: - SparkampModel

/// Single source of truth that bridges every FFI call to @Published SwiftUI state.
/// All mutations happen on the main thread; this class is @MainActor.
@MainActor
final class SparkampModel: ObservableObject {

    // MARK: Published state

    @Published var isPlaying = false
    @Published var isPaused  = false
    @Published var position: Double = 0      // seconds
    @Published var duration: Double = -1     // seconds, -1 = unknown
    @Published var currentTitle  = ""
    @Published var currentArtist = ""
    @Published var volume: Double = 1.0      // 0–1
    @Published var repeatMode: Int = 0       // 0=Off 1=One 2=All
    @Published var shuffleEnabled = false
    @Published var playlistItems: [PlaylistItem] = []
    @Published var currentIndex: Int = -1
    /// Non-nil when GStreamer failed to initialise (ctx is null). Shows install instructions.
    @Published var fatalError: String? = nil
    /// Non-nil when a runtime playback error fires from the GStreamer bus.
    @Published var playbackError: String? = nil
    @Published var playlistVisible: Bool = false
    /// When true, the keyboard shortcuts window is open.
    @Published var keyboardShortcutsVisible: Bool = false
    /// When true, the LCD time display shows remaining time as a negative value.
    @Published var showRemainingTime: Bool = false
    /// Current visualizer mode mirrored from config: 0 = Bars, 1 = Waveform.
    @Published var vizMode: Int = 0
    /// When true, the fullscreen visualizer window is open.
    @Published var fullscreenVizVisible: Bool = false
    /// When true, the jump-to-track overlay is open.
    @Published var jumpToTrackVisible: Bool = false
    /// When true, the equalizer window is open.
    @Published var equalizerVisible: Bool = false
    /// When true, the settings window is open.
    @Published var settingsVisible: Bool = false
    /// When true, the ID3 tag editor window is open.
    @Published var id3EditorVisible: Bool = false
    /// Playlist index to open in the ID3 editor; -1 means the current track.
    @Published var id3TrackIndex: Int = -1
    /// When set, the ID3 editor opens this file path directly (bypasses playlist index).
    @Published var id3DirectPath: String = ""
    /// Artwork image currently shown in the ID3 editor (shared with the artwork zoom window).
    @Published var artworkImage: NSImage? = nil
    /// When true, the artwork zoom window is open.
    @Published var artworkWindowVisible: Bool = false

    // ── Media Library ────────────────────────────────────────────────────────
    @Published var mediaLibraryVisible: Bool = false
    /// Tracks currently shown in the ML window (all or filtered by query).
    @Published var mlTracks: [MLTrack] = []
    /// Watched folder paths.
    @Published var mlFolders: [String] = []
    /// Saved playlists in the library DB.
    @Published var mlSavedPlaylists: [MLPlaylistItem] = []
    /// True while a background scan is running.
    @Published var mlScanRunning: Bool = false
    @Published var mlScanDone: Int = 0
    @Published var mlScanTotal: Int = 0
    /// True once `sparkamp_ml_open` has been called.
    var mlIsOpen: Bool = false
    /// Counts ticks while a scan is running; used to throttle intermediate reloads.
    private var mlScanTickCount: Int = 0

    // ── Deduplication ────────────────────────────────────────────────────────
    @Published var dedupVisible: Bool = false
    @Published var dedupGroups: [DedupGroupItem] = []
    @Published var dedupRunning: Bool = false
    @Published var dedupGroupTotal: Int = 0
    private var dedupCtxPtr: OpaquePointer? = nil

    // MARK: Private — background scan tracking

    /// Set to `Date()` whenever files are added; the tick polls for incomplete
    /// data (missing duration or metadata) for up to `scanWindowSeconds` after
    /// the last add, regardless of whether dirty_count fired.
    private var lastAddTime: Date? = nil
    private let scanWindowSeconds: TimeInterval = 15.0

    /// Raw pointer to the Rust SparkampCtx.
    /// Internal (not private) so Canvas-based visualizer views can call FFI
    /// directly at 30 fps without routing data through @Published properties.
    var ctx: OpaquePointer?
    private var tickTimer: Timer?
    private var keyMonitor: Any?

    // MARK: Init / deinit

    init() {
        ctx = sparkamp_create()

        guard ctx != nil else {
            fatalError = "Sparkamp could not initialise GStreamer."
            return
        }

        setupCallbacks()
        // Restore Swift-side UI state
        playlistVisible      = UserDefaults.standard.bool(forKey: "sparkamp.playlistVisible")
        equalizerVisible     = UserDefaults.standard.bool(forKey: "sparkamp.equalizerVisible")
        mediaLibraryVisible  = UserDefaults.standard.bool(forKey: "sparkamp.mlVisible")
        refreshAll()
        startTick()
        startKeyMonitor()

        // Save on graceful quit (Cmd+Q / applicationWillTerminate).
        // Note: Xcode's Stop button sends SIGKILL — no cleanup runs in that case.
        NotificationCenter.default.addObserver(
            forName: NSApplication.willTerminateNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            guard let self else { return }
            // queue: .main guarantees main-thread delivery; assumeIsolated
            // satisfies the compiler's Sendable check without a Task hop.
            MainActor.assumeIsolated {
                // Save full state (Rust config + Swift UserDefaults) at quit time
                // so window visibility is correctly restored on next launch.
                self.saveState()
            }
        }
    }

    deinit {
        tickTimer?.invalidate()
        if let monitor = keyMonitor { NSEvent.removeMonitor(monitor) }
        if let ctx = ctx { sparkamp_save_config(ctx) }
        sparkamp_destroy(ctx)
    }

    // MARK: Tick

    private func startTick() {
        tickTimer = Timer.scheduledTimer(withTimeInterval: 0.1, repeats: true) { [weak self] _ in
            guard let self = self else { return }
            Task { @MainActor in self.tick() }
        }
    }

    private func tick() {
        guard let ctx = ctx else { return }
        sparkamp_tick(ctx)

        // Sync lightweight state that changes during playback.
        let state = sparkamp_get_state(ctx)
        isPlaying = (state == 1)
        isPaused  = (state == 2)
        position  = sparkamp_get_position(ctx)
        duration  = sparkamp_get_duration(ctx)
        let idx   = Int(sparkamp_playlist_current_index(ctx))
        if idx != currentIndex {
            currentIndex = idx
            refreshCurrentTrackInfo()
        }

        // Poll for background scan results in two cases:
        //  1. dirty_count > 0 — Rust applied at least one metadata or duration
        //     update this tick (fast path; always triggers when scans land).
        //  2. Within the scan window — keeps polling even if dirty_count is 0,
        //     which handles formats where Symphonia + Discoverer take a few ticks
        //     to return OR where the probe result lands between tick boundaries.
        let dirty = Int(sparkamp_take_playlist_dirty_count(ctx))
        let scanActive = lastAddTime.map { Date().timeIntervalSince($0) < scanWindowSeconds } ?? false
        if dirty > 0 || scanActive {
            refreshDirtyPlaylistItems()
        }

        // Keep vizMode in sync so views can observe it reactively.
        let newVizMode = Int(sparkamp_get_viz_mode(ctx))
        if newVizMode != vizMode { vizMode = newVizMode }

        // Poll media library scan progress (if running).
        if mlScanRunning {
            let stillRunning = sparkamp_ml_scan_is_running(ctx) != 0
            var done: Int32 = 0, total: Int32 = 0
            sparkamp_ml_scan_progress(ctx, &done, &total)
            mlScanDone  = Int(done)
            mlScanTotal = Int(total)
            mlScanTickCount += 1
            // Refresh the track list every ~1 s so metadata fills in live.
            if mlScanTickCount % 10 == 0 {
                mlFetchTracks()
            }
            if !stillRunning {
                mlScanRunning = false
                mlScanTickCount = 0
                mlRefreshFolders()
                mlFetchTracks()
            }
        }
    }

    /// Re-read every playlist row that still has incomplete data (missing
    /// duration or placeholder title/artist), then write the whole array back
    /// in a single assignment so SwiftUI triggers exactly one re-render.
    /// Once all background scans have landed this becomes a cheap no-op:
    /// the inner guard skips every complete row without any FFI call.
    private func refreshDirtyPlaylistItems() {
        guard let ctx = ctx else { return }
        let count = Int(sparkamp_playlist_len(ctx))
        guard count == playlistItems.count else {
            // Playlist length changed while we were scanning — full rebuild.
            refreshPlaylist()
            return
        }

        var newItems = playlistItems
        var changed  = false

        for i in 0..<count {
            let item = newItems[i]
            // Skip rows that are already complete — no FFI call needed.
            guard item.duration < 0 || item.artist.isEmpty else { continue }

            let titlePtr       = sparkamp_playlist_get_title(ctx, Int32(i))
            let artistPtr      = sparkamp_playlist_get_artist(ctx, Int32(i))
            let albumArtistPtr = sparkamp_playlist_get_album_artist(ctx, Int32(i))
            let newTitle       = titlePtr.map       { String(cString: $0) } ?? ""
            let newArtist      = artistPtr.map      { String(cString: $0) } ?? ""
            let newAlbumArtist = albumArtistPtr.map { String(cString: $0) } ?? ""
            sparkamp_free_string(titlePtr)
            sparkamp_free_string(artistPtr)
            sparkamp_free_string(albumArtistPtr)
            let newDuration    = sparkamp_playlist_get_duration(ctx, Int32(i))

            if newTitle != item.title || newArtist != item.artist
                || newAlbumArtist != item.albumArtist || newDuration != item.duration {
                newItems[i] = PlaylistItem(
                    id: i,
                    title: newTitle,
                    artist: newArtist,
                    albumArtist: newAlbumArtist,
                    duration: newDuration,
                    broken: sparkamp_playlist_is_broken(ctx, Int32(i)) != 0,
                    readOnly: item.readOnly,        // read-only status doesn't change mid-scan
                    fileMissing: item.fileMissing   // idem
                )
                changed = true
            }
        }

        if changed {
            playlistItems = newItems     // single assignment → one SwiftUI re-render
            refreshCurrentTrackInfo()
        }
    }

    // MARK: Callbacks

    private func setupCallbacks() {
        guard let ctx = ctx else { return }
        let selfPtr = Unmanaged.passUnretained(self).toOpaque()

        // EOS: auto-advance to the next track.
        sparkamp_set_eos_callback(ctx, { userdata in
            guard let userdata = userdata else { return }
            let model = Unmanaged<SparkampModel>.fromOpaque(userdata).takeUnretainedValue()
            model.handleEOS()
        }, selfPtr)

        // Error: mark the current track broken and skip to the next one.
        // Broken tracks show an X indicator in the playlist; no popup is shown.
        sparkamp_set_error_callback(ctx, { userdata, _ in
            guard let userdata = userdata else { return }
            let model = Unmanaged<SparkampModel>.fromOpaque(userdata).takeUnretainedValue()
            model.handlePlaybackError()
        }, selfPtr)

        // Position: update seek bar and duration display.
        sparkamp_set_position_callback(ctx, { userdata, pos, dur in
            guard let userdata = userdata else { return }
            let model = Unmanaged<SparkampModel>.fromOpaque(userdata).takeUnretainedValue()
            model.position = pos
            model.duration = dur
        }, selfPtr)
    }

    private func handleEOS() {
        guard let ctx = ctx else { return }
        sparkamp_advance_after_eos(ctx)
        refreshAll()
        saveState()
    }

    private func handlePlaybackError() {
        guard let ctx = ctx else { return }
        // Mark the current track broken so the playlist shows the X indicator.
        let idx = sparkamp_playlist_current_index(ctx)
        if idx >= 0 {
            sparkamp_playlist_mark_broken(ctx, idx)
        }
        // Advance past the broken track the same way EOS does (respects repeat/shuffle).
        sparkamp_advance_after_eos(ctx)
        refreshAll()
    }

    // MARK: Refresh helpers

    func refreshAll() {
        guard let ctx = ctx else { return }
        volume         = sparkamp_get_volume(ctx)
        repeatMode     = Int(sparkamp_get_repeat_mode(ctx))
        shuffleEnabled = sparkamp_get_shuffle(ctx) != 0
        currentIndex   = Int(sparkamp_playlist_current_index(ctx))
        refreshPlaylist()
        refreshCurrentTrackInfo()
    }

    func refreshPlaylist() {
        guard let ctx = ctx else { return }
        let count = Int(sparkamp_playlist_len(ctx))
        playlistItems = (0..<count).map { i in
            let titlePtr       = sparkamp_playlist_get_title(ctx, Int32(i))
            let artistPtr      = sparkamp_playlist_get_artist(ctx, Int32(i))
            let albumArtistPtr = sparkamp_playlist_get_album_artist(ctx, Int32(i))
            let title       = titlePtr.map       { String(cString: $0) } ?? ""
            let artist      = artistPtr.map      { String(cString: $0) } ?? ""
            let albumArtist = albumArtistPtr.map { String(cString: $0) } ?? ""
            sparkamp_free_string(titlePtr)
            sparkamp_free_string(artistPtr)
            sparkamp_free_string(albumArtistPtr)
            return PlaylistItem(
                id: i,
                title: title,
                artist: artist,
                albumArtist: albumArtist,
                duration: sparkamp_playlist_get_duration(ctx, Int32(i)),
                broken: sparkamp_playlist_is_broken(ctx, Int32(i)) != 0,
                readOnly: sparkamp_playlist_is_read_only(ctx, Int32(i)) != 0,
                fileMissing: sparkamp_playlist_file_missing(ctx, Int32(i)) != 0
            )
        }
    }

    private func refreshCurrentTrackInfo() {
        guard let ctx = ctx else { return }
        let idx = Int(sparkamp_playlist_current_index(ctx))
        if idx >= 0, idx < playlistItems.count {
            currentTitle  = playlistItems[idx].title.isEmpty ? "Unknown" : playlistItems[idx].title
            let a = playlistItems[idx].artist
            let aa = playlistItems[idx].albumArtist
            currentArtist = a.isEmpty ? aa : a
        } else {
            currentTitle  = ""
            currentArtist = ""
        }
    }

    // MARK: Transport actions

    func play()  { if let ctx = ctx { sparkamp_play(ctx);  tick() } }
    func pause() { if let ctx = ctx { sparkamp_pause(ctx); tick() } }
    func stop()  { if let ctx = ctx { sparkamp_stop(ctx);  tick() } }

    func togglePlay() {
        if isPlaying { pause() } else { play() }
    }

    func next() {
        guard let ctx = ctx else { return }
        sparkamp_nav_next(ctx)
        refreshAll()
        saveState()
    }

    func prev() {
        guard let ctx = ctx else { return }
        sparkamp_nav_prev(ctx)
        refreshAll()
        saveState()
    }

    func seek(to fraction: Double) {
        guard let ctx = ctx else { return }
        sparkamp_seek(ctx, fraction)
    }

    func setVolume(_ vol: Double) {
        guard let ctx = ctx else { return }
        sparkamp_set_volume(ctx, vol)
        volume = sparkamp_get_volume(ctx)
    }

    func adjustVolume(by delta: Double) {
        setVolume((volume + delta).clamped(to: 0...1))
    }

    func cycleRepeat() {
        guard let ctx = ctx else { return }
        sparkamp_cycle_repeat(ctx)
        repeatMode = Int(sparkamp_get_repeat_mode(ctx))
        saveState()
    }

    func toggleShuffle() {
        guard let ctx = ctx else { return }
        sparkamp_toggle_shuffle(ctx)
        shuffleEnabled = sparkamp_get_shuffle(ctx) != 0
        saveState()
    }

    func toggleRemainingTime() {
        showRemainingTime.toggle()
    }

    func toggleKeyboardShortcuts() {
        keyboardShortcutsVisible.toggle()
    }

    func cycleVizMode() {
        guard let ctx = ctx else { return }
        sparkamp_cycle_viz_mode(ctx)
        vizMode = Int(sparkamp_get_viz_mode(ctx))
    }

    func openFullscreenViz() {
        if fullscreenVizVisible { closeFullscreenViz(); return }
        guard let ctx = ctx, sparkamp_get_viz_mode(ctx) == 1 else { return }
        fullscreenVizVisible = true
    }

    func openId3Editor(trackIndex: Int = -1) {
        id3TrackIndex = trackIndex
        id3EditorVisible = true
    }

    func closeFullscreenViz() {
        // Exit OS fullscreen before SwiftUI dismisses the window so the
        // animation completes cleanly.  Finding by styleMask is reliable;
        // SwiftUI WindowGroup doesn't expose the NSWindow directly.
        if let win = NSApp.windows.first(where: { $0.styleMask.contains(.fullScreen) }) {
            win.toggleFullScreen(nil)
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.7) {
                self.fullscreenVizVisible = false
            }
        } else {
            fullscreenVizVisible = false
        }
    }

    func jumpTo(index: Int) {
        guard let ctx = ctx else { return }
        sparkamp_playlist_jump(ctx, Int32(index))
        refreshAll()
        saveState()
    }

    // MARK: Playlist actions

    func addFiles(_ urls: [URL]) {
        guard let ctx = ctx else { return }

        // If "Replace playlist" is the configured behavior, clear before adding.
        let shouldReplace = Int(sparkamp_get_playlist_add_behavior(ctx)) == 1
        if shouldReplace {
            sparkamp_playlist_clear(ctx)
        }

        // Indices of tracks we fast-added — we'll scan just those.
        var newIndices: [Int] = []

        for url in urls {
            var isDir: ObjCBool = false
            FileManager.default.fileExists(atPath: url.path, isDirectory: &isDir)

            if isDir.boolValue {
                // Folder: use the existing recursive-scan path (adds all audio
                // files found under the folder, reads full tags — acceptable here
                // because folder scans are done by the user deliberately and the
                // existing implementation already handles this path).
                let countBefore = Int(sparkamp_playlist_len(ctx))
                url.path.withCString { sparkamp_playlist_add(ctx, $0) }
                let countAfter = Int(sparkamp_playlist_len(ctx))
                newIndices.append(contentsOf: countBefore..<countAfter)
            } else {
                // Individual file: fast-add (filename as placeholder, no ID3 read).
                // sparkamp_playlist_add_fast returns the new track's index or -1.
                let idx = url.path.withCString { sparkamp_playlist_add_fast(ctx, $0) }
                if idx >= 0 { newIndices.append(Int(idx)) }
            }
        }

        // Show the playlist immediately — new tracks appear with their filename
        // stems as placeholder titles before background scanning completes.
        refreshPlaylist()

        // Kick off background scans for every newly added track:
        //   sparkamp_scan_metadata  — reads ID3/Vorbis on a Rayon thread
        //   sparkamp_probe_duration — reads container header on a Rayon thread
        // Both write results to Arc<Mutex<>> queues; sparkamp_tick drains them
        // each 100 ms tick and increments dirty_count so Swift knows to refresh.
        for i in newIndices {
            sparkamp_scan_metadata(ctx, Int32(i))
            sparkamp_probe_duration(ctx, Int32(i))
        }

        // Mark the start of the scan window so tick() keeps polling for
        // incomplete rows even if dirty_count hasn't fired yet.
        if !newIndices.isEmpty {
            lastAddTime = Date()

            // Auto-play the first newly added track if configured to do so.
            if sparkamp_get_autoplay_on_add(ctx) {
                sparkamp_playlist_jump(ctx, Int32(newIndices[0]))
                sparkamp_play(ctx)
                refreshCurrentTrackInfo()
            }

            saveState()
        }
    }

    func removeTrack(at index: Int) {
        guard let ctx = ctx else { return }
        sparkamp_playlist_remove(ctx, Int32(index))
        refreshPlaylist()
        saveState()
    }

    func moveTrack(from: IndexSet, to: Int) {
        guard let ctx = ctx, let source = from.first else { return }
        let dest = source < to ? to - 1 : to
        sparkamp_playlist_move(ctx, Int32(source), Int32(dest))
        refreshPlaylist()
        saveState()
    }

    func clearPlaylist() {
        guard let ctx = ctx else { return }
        sparkamp_playlist_clear(ctx)
        refreshPlaylist()
        saveState()
    }

    // MARK: File picker

    func openFilePicker() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = true
        panel.allowedContentTypes = [.audio]
        panel.begin { [weak self] response in
            guard response == .OK, let self = self else { return }
            Task { @MainActor in self.addFiles(panel.urls) }
        }
    }

    func openFolderPicker() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.begin { [weak self] response in
            guard response == .OK, let self = self else { return }
            Task { @MainActor in self.addFiles(panel.urls) }
        }
    }

    // MARK: Persistence

    /// Flush Rust-side config + playlist to disk and persist Swift-side UI
    /// state in UserDefaults.  Called after every meaningful state change so
    /// the most recent state survives an unexpected kill (e.g. Xcode stop).
    func saveState() {
        if let ctx = ctx { sparkamp_save_config(ctx) }
        UserDefaults.standard.set(playlistVisible,     forKey: "sparkamp.playlistVisible")
        UserDefaults.standard.set(equalizerVisible,    forKey: "sparkamp.equalizerVisible")
        UserDefaults.standard.set(mediaLibraryVisible, forKey: "sparkamp.mlVisible")
    }

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
            return MLPlaylistItem(id: dbId, name: String(cString: ptr))
        }
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
    /// Respects the "append vs. replace" playback setting and always plays.
    func mlDoubleClickTracks(ids: [Int64]) {
        guard let ctx = ctx else { return }
        let shouldReplace = Int(sparkamp_get_playlist_add_behavior(ctx)) == 1
        let indexBefore = Int(sparkamp_playlist_len(ctx))
        if shouldReplace {
            clearPlaylist()
            mlAddToPlaylist(ids: ids)
            sparkamp_playlist_jump(ctx, 0)
        } else {
            mlAddToPlaylist(ids: ids)
            sparkamp_playlist_jump(ctx, Int32(indexBefore))
        }
        sparkamp_play(ctx)
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
                artworkImage = image
                artworkWindowVisible = true
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

    func mlOpenTagEditorForPath(_ path: String) {
        id3DirectPath = path
        id3EditorVisible = true
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

    /// Delete a playlist by row ID (DB only; .m3u file is kept on disk).
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

    /// Overwrite a saved playlist's .m3u file with the given ordered track IDs.
    func mlSavePlaylist(id: Int64, trackIds: [Int64]) {
        guard let ctx = ctx else { return }
        var ids = trackIds
        ids.withUnsafeMutableBufferPointer { buf in
            sparkamp_ml_save_playlist(ctx, id, buf.baseAddress, Int32(trackIds.count))
        }
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

    // MARK: Deduplication

    func startDedup() {
        guard let ctx = ctx, mlIsOpen else { return }
        dedupGroups = []
        dedupRunning = true
        dedupGroupTotal = 0

        let selfAddr = Unmanaged.passUnretained(self).toOpaque()
        let selfAddrInt = Int(bitPattern: selfAddr)

        dedupCtxPtr = sparkamp_dedup_start(ctx,
            // group_cb — called from a Rayon thread
            { ud, groupPtr in
                guard let groupPtr else { return }
                let group = groupPtr.pointee
                var items: [DedupTrackItem] = []
                for i in 0..<Int(group.track_count) {
                    var t = group.tracks[i]
                    let p = cBytesToString(&t.path)
                    items.append(DedupTrackItem(
                        id: p,
                        path: p,
                        title:  cBytesToString(&t.title),
                        artist: cBytesToString(&t.artist),
                        durationSecs: t.duration_secs
                    ))
                }
                let conf = Int(group.confidence)
                let newGroup = DedupGroupItem(id: UUID(), confidence: conf, tracks: items)
                let modelAddr = Int(bitPattern: ud)
                DispatchQueue.main.async {
                    let model = Unmanaged<SparkampModel>
                        .fromOpaque(UnsafeRawPointer(bitPattern: modelAddr)!)
                        .takeUnretainedValue()
                    MainActor.assumeIsolated { model.dedupGroups.append(newGroup) }
                }
            },
            // done_cb
            { ud, totalCount in
                let modelAddr = Int(bitPattern: ud)
                DispatchQueue.main.async {
                    let model = Unmanaged<SparkampModel>
                        .fromOpaque(UnsafeRawPointer(bitPattern: modelAddr)!)
                        .takeUnretainedValue()
                    MainActor.assumeIsolated {
                        model.dedupRunning = false
                        model.dedupGroupTotal = Int(totalCount)
                    }
                }
            },
            UnsafeMutableRawPointer(bitPattern: selfAddrInt)
        )
    }

    func cancelDedup() {
        if let dctx = dedupCtxPtr {
            sparkamp_dedup_cancel(dctx)
        }
    }

    func freeDedup() {
        if let dctx = dedupCtxPtr {
            sparkamp_dedup_free(dctx)
            dedupCtxPtr = nil
        }
    }

    func dedupAddGroupToPlaylist(_ group: DedupGroupItem) {
        guard let ctx = ctx else { return }
        var ptrs: [UnsafePointer<CChar>?] = group.tracks.map { _ in nil }
        let cStrings = group.tracks.map { ($0.path as NSString).utf8String! }
        ptrs = cStrings.map { $0 }
        ptrs.withUnsafeMutableBufferPointer { buf in
            sparkamp_dedup_add_to_playlist(ctx, buf.baseAddress, Int32(group.tracks.count))
        }
        refreshPlaylist()
    }

    func dedupReplacePlaylistWithGroup(_ group: DedupGroupItem) {
        guard let ctx = ctx else { return }
        var ptrs: [UnsafePointer<CChar>?] = group.tracks.map { ($0.path as NSString).utf8String }
        ptrs.withUnsafeMutableBufferPointer { buf in
            sparkamp_dedup_replace_playlist(ctx, buf.baseAddress, Int32(group.tracks.count))
        }
        refreshAll()
    }

    func openInFinder(_ path: String) {
        path.withCString { sparkamp_open_file_location($0) }
    }

    // MARK: Keyboard shortcuts

    private func startKeyMonitor() {
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            guard let self = self else { return event }
            // Don't intercept keys when a text field has focus — SwiftUI's
            // TextField is backed by NSTextView on macOS, so this one check
            // covers all text inputs (jump-to-track search, etc.).
            if NSApp.keyWindow?.firstResponder is NSTextView { return event }
            let chars   = event.charactersIgnoringModifiers
            let keyCode = event.keyCode
            let hasMods = !event.modifierFlags
                .intersection([.command, .option, .control])
                .isEmpty
            let consumed = MainActor.assumeIsolated {
                self.handleRawKey(chars: chars, keyCode: keyCode, hasModifiers: hasMods)
            }
            return consumed ? nil : event
        }
    }

    /// Handle a key expressed as plain Sendable values. Returns true if consumed.
    @discardableResult
    func handleRawKey(chars: String?, keyCode: UInt16, hasModifiers: Bool) -> Bool {
        guard !hasModifiers, let chars = chars else { return false }

        switch chars {
        case "z": prev();          return true
        case "x": play();          return true
        case "c": togglePlay();    return true
        case "v": stop();          return true
        case "b": next();          return true
        case "r": cycleRepeat();               return true
        case "s": toggleShuffle();             return true
        case "-": adjustVolume(by: -0.05);    return true
        case "=": adjustVolume(by:  0.05);    return true
        case "p":
            playlistVisible.toggle()
            UserDefaults.standard.set(playlistVisible, forKey: "sparkamp.playlistVisible")
            return true
        case "i": toggleKeyboardShortcuts();  return true
        case "a": cycleVizMode();             return true
        case "f": openFullscreenViz();        return true  // toggles open/close
        case "j": jumpToTrackVisible.toggle(); return true
        case "u": equalizerVisible.toggle(); saveState(); return true
        case "d":
            id3TrackIndex = -1  // current track
            id3EditorVisible = true
            return true
        case "\u{1B}":  // Escape — close fullscreen if open
            if fullscreenVizVisible { closeFullscreenViz(); return true }
            return false
        default: break
        }

        // Arrow keys — left/right seek ±5 s, up/down adjust volume
        switch keyCode {
        case 123: seek(to: ((position - 5) / max(duration, 1)).clamped(to: 0...1)); return true
        case 124: seek(to: ((position + 5) / max(duration, 1)).clamped(to: 0...1)); return true
        case 125: adjustVolume(by: -0.05); return true  // down arrow
        case 126: adjustVolume(by:  0.05); return true  // up arrow
        default: break
        }

        return false
    }
}

// MARK: - Comparable clamping helper

extension Comparable {
    func clamped(to range: ClosedRange<Self>) -> Self {
        min(max(self, range.lowerBound), range.upperBound)
    }
}
