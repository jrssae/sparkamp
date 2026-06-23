import Foundation
import AppKit
import IOKit.pwr_mgt

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
    @Published var fullscreenVizVisible: Bool = false {
        // Single chokepoint for the display-sleep assertion: every open and
        // close path (f key, Esc, double-click, onDisappear) flips this flag.
        didSet { updateDisplaySleepAssertion() }
    }
    /// Incremented whenever the now-playing track (re)starts — track change
    /// via next/prev, play after pause/stop, or auto-advance. The fullscreen
    /// visualizer observes this to (re)show its track toast even when the
    /// title is unchanged. See `announceNowPlaying()`.
    @Published var nowPlayingNonce: Int = 0
    /// FPS overlay in the fullscreen visualizer (`g` key). Lives on the model
    /// because the app-wide key monitor handles the keypress — SwiftUI
    /// `.onKeyPress` on the fullscreen view never fires for keys the monitor
    /// doesn't pass through, and focus there is unreliable anyway.
    @Published var fullscreenFpsVisible: Bool = false
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
    /// Bumps every time the model writes back to the library DB (e.g. a
    /// play_count increment from `record_play`).  The Media Library window
    /// observes this and re-runs its own filtered/sorted fetch so the
    /// table reflects the new value without resetting search or sort.
    @Published var mlReloadTrigger: Int = 0
    /// Bumps every time a saved playlist's *contents* (the playlist file on disk)
    /// change — e.g. append-paths, save, save-as.  The playlist editor
    /// observes this so right-click "Add to Playlist" from the active
    /// playlist (or any other path-level mutation) reflects in the editor
    /// without manual reload.
    @Published var mlPlaylistContentTrigger: Int = 0
    /// True once `sparkamp_ml_open` has been called.
    var mlIsOpen: Bool = false
    /// Counts ticks while a scan is running; used to throttle intermediate reloads.
    private var mlScanTickCount: Int = 0

    // ── Devices (external storage) ─────────────────────────────────────────
    /// Connected removable devices, refreshed by the ~2 s poll while the Media
    /// Library window is open. Keyed for selection by `backendId` (BSD name).
    @Published var devices: [Device] = []
    /// The device currently shown in the detail view (its BSD name), or nil for
    /// the overview.
    @Published var selectedDeviceBSD: String? = nil
    /// Song / playlist counts per device id, filled lazily for the overview.
    @Published var deviceCounts: [String: DeviceCounts] = [:]
    /// Ticks counted only while the ML window is open; gates the 2 s device poll.
    var deviceTickCount: Int = 0

    // ── Deduplication ────────────────────────────────────────────────────────
    @Published var dedupVisible: Bool = false
    @Published var dedupGroups: [DedupGroupItem] = []
    @Published var dedupRunning: Bool = false
    @Published var dedupGroupTotal: Int = 0
    // Internal (not private): owned here, used by SparkampModel+Dedupe.swift.
    var dedupCtxPtr: OpaquePointer? = nil

    // MARK: Private — background scan tracking

    /// Set to `Date()` whenever files are added; the tick polls for incomplete
    /// data (missing duration or metadata) for up to `scanWindowSeconds` after
    /// the last add, regardless of whether dirty_count fired.
    var lastAddTime: Date? = nil
    private let scanWindowSeconds: TimeInterval = 15.0

    // MARK: Private — play-count gating
    //
    // Mirrors the GTK frontend rule: a track only counts as "played" once
    // its position passes the threshold below.  Tracking the path (not just
    // the playlist index) prevents re-counting if the same file appears
    // twice in the queue or if the playlist is rebuilt mid-track.
    private var countedPlayPath: String? = nil
    private let playCountThresholdSecs: Double = 20.0
    /// Last raw playback state observed by tick() — used to detect
    /// stopped→playing transitions so a replay re-arms the play-count gate.
    /// 0 = stopped, 1 = playing, 2 = paused (matches sparkamp_get_state).
    private var lastPlaybackState: Int32 = 0

    /// Raw pointer to the Rust SparkampCtx.
    /// Internal (not private) so Canvas-based visualizer views can call FFI
    /// directly at 30 fps without routing data through @Published properties.
    var ctx: OpaquePointer?
    private var tickTimer: Timer?
    // Internal (not private): installed here, torn down/queried from
    // SparkampModel+Keys.swift.
    var keyMonitor: Any?
    /// ID of the held "prevent display sleep" power assertion; 0 = none.
    /// Stored here (extensions cannot hold stored properties); managed by
    /// updateDisplaySleepAssertion() in SparkampModel+Transport.swift.
    var displaySleepAssertion: IOPMAssertionID = 0

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

        // Exit fullscreen when the display sleeps anyway (manual sleep, or
        // keep-awake is off): on wake, macOS otherwise bounces focus between
        // the main Space and the fullscreen visualizer Space.
        NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.screensDidSleepNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            guard let self else { return }
            MainActor.assumeIsolated {
                if self.fullscreenVizVisible { self.closeFullscreenViz() }
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

    func tick() {
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
            // New track started — reset the play-count gate so the next
            // record_play fires once playback crosses the threshold.
            countedPlayPath = nil
        }

        // Detect a stopped→playing transition for the same track (a replay).
        // sparkamp_get_state returns 0 = stopped, 1 = playing, 2 = paused.
        // Pause→play deliberately does NOT reset the gate (we don't want a
        // mid-track pause to double-count); only a hard stop and re-press
        // of Play should arm a fresh count.
        if lastPlaybackState == 0 && state == 1 {
            countedPlayPath = nil
        }
        lastPlaybackState = state

        // Record a play in the media library after the user has listened
        // for `playCountThresholdSecs` seconds of the current track.  The
        // path-based gate (countedPlayPath) ensures we only count each
        // playthrough once even if tick() runs many times per second.
        if isPlaying, idx >= 0, position >= playCountThresholdSecs {
            if let pathPtr = sparkamp_playlist_get_path(ctx, Int32(idx)) {
                let path = String(cString: pathPtr)
                sparkamp_free_string(pathPtr)
                if !path.isEmpty, countedPlayPath != path {
                    path.withCString { sparkamp_ml_record_play(ctx, $0) }
                    countedPlayPath = path
                    // Nudge the Media Library window to re-run its own
                    // filtered/sorted fetch so the row's play count and
                    // last-played timestamp update live.
                    if mediaLibraryVisible { mlReloadTrigger &+= 1 }
                }
            }
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

        // Poll connected devices ~every 2 s while the ML window is open. The
        // tick fires at 10 Hz, so every 20th tick. Detection only — counts are
        // computed on demand for the overview (refreshDeviceCounts).
        if mediaLibraryVisible {
            deviceTickCount += 1
            if deviceTickCount % 20 == 1 {
                pollDevices()
            }
        } else if deviceTickCount != 0 {
            deviceTickCount = 0
        }

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
        announceNowPlaying()
    }

    /// Bump the now-playing nonce so the fullscreen visualizer re-shows its
    /// track toast. A nonce (not `currentTitle`) is the trigger because the
    /// toast must also fire when the SAME track restarts — play after a
    /// pause or stop — where the title never changes.
    func announceNowPlaying() {
        nowPlayingNonce &+= 1
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
        announceNowPlaying()
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

    func refreshCurrentTrackInfo() {
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

}
