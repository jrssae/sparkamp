import Foundation
import AppKit

// MARK: - Data types

struct PlaylistItem: Identifiable {
    let id: Int          // the playlist index
    let title: String
    let artist: String
    let albumArtist: String
    let duration: Double // seconds, -1 = unknown
    let broken: Bool

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
    /// When true, the LCD time display shows remaining time as a negative value.
    @Published var showRemainingTime: Bool = false

    // MARK: Private

    /// Raw pointer to the Rust SparkampCtx.
    private var ctx: OpaquePointer?
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
        refreshAll()
        startTick()
        startKeyMonitor()
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
        // Keep the current track's duration in playlistItems up-to-date as
        // GStreamer reports it (it may not be available when the track is added).
        if idx >= 0, idx < playlistItems.count {
            let liveDur = sparkamp_playlist_get_duration(ctx, Int32(idx))
            if liveDur >= 0, playlistItems[idx].duration < 0 {
                let item = playlistItems[idx]
                playlistItems[idx] = PlaylistItem(
                    id: item.id,
                    title: item.title,
                    artist: item.artist,
                    albumArtist: item.albumArtist,
                    duration: liveDur,
                    broken: item.broken
                )
            }
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

        // Error: surface in the UI as a dismissable playback error (not a fatal alert).
        sparkamp_set_error_callback(ctx, { userdata, msg in
            guard let userdata = userdata else { return }
            let model = Unmanaged<SparkampModel>.fromOpaque(userdata).takeUnretainedValue()
            let str = msg.flatMap { String(cString: $0) } ?? "Unknown playback error"
            model.playbackError = str
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
                broken: sparkamp_playlist_is_broken(ctx, Int32(i)) != 0
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
    }

    func prev() {
        guard let ctx = ctx else { return }
        sparkamp_nav_prev(ctx)
        refreshAll()
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
    }

    func toggleShuffle() {
        guard let ctx = ctx else { return }
        sparkamp_toggle_shuffle(ctx)
        shuffleEnabled = sparkamp_get_shuffle(ctx) != 0
    }

    func toggleRemainingTime() {
        showRemainingTime.toggle()
    }

    func jumpTo(index: Int) {
        guard let ctx = ctx else { return }
        sparkamp_playlist_jump(ctx, Int32(index))
        refreshAll()
    }

    // MARK: Playlist actions

    func addFiles(_ urls: [URL]) {
        guard let ctx = ctx else { return }
        for url in urls {
            url.path.withCString { sparkamp_playlist_add(ctx, $0) }
        }
        refreshPlaylist()
        // Probe durations for any tracks that don't have them yet.
        let count = Int(sparkamp_playlist_len(ctx))
        for i in 0..<count {
            if sparkamp_playlist_get_duration(ctx, Int32(i)) < 0 {
                sparkamp_probe_duration(ctx, Int32(i))
            }
        }
    }

    func removeTrack(at index: Int) {
        guard let ctx = ctx else { return }
        sparkamp_playlist_remove(ctx, Int32(index))
        refreshPlaylist()
    }

    func moveTrack(from: IndexSet, to: Int) {
        guard let ctx = ctx, let source = from.first else { return }
        let dest = source < to ? to - 1 : to
        sparkamp_playlist_move(ctx, Int32(source), Int32(dest))
        refreshPlaylist()
    }

    func clearPlaylist() {
        guard let ctx = ctx else { return }
        sparkamp_playlist_clear(ctx)
        refreshPlaylist()
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

    // MARK: Keyboard shortcuts

    private func startKeyMonitor() {
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            guard let self = self else { return event }
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
        case "r": cycleRepeat();   return true
        case "s": toggleShuffle(); return true
        case "-": adjustVolume(by: -0.05); return true
        case "=": adjustVolume(by:  0.05); return true
        case "p": playlistVisible.toggle(); return true
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
