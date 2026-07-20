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
    /// Presented fullscreen-visualizer frames since launch. Deliberately NOT
    /// @Published — it changes every frame (up to display rate) and must
    /// never trigger layout; the FPS overlay's low-rate sampler reads the
    /// delta. Wraps on overflow (&+), which the delta math tolerates.
    var vizFrameCount: UInt64 = 0
    /// Smoothed cost of one fullscreen Granite render+present, in ms (plain
    /// var, same non-published rationale). Shown in the FPS overlay to tell
    /// "our callback overruns the frame budget" apart from "the system
    /// throttled the display link" when the rate reads low.
    var vizRenderMs: Double = 0

    /// Record one presented fullscreen-visualizer frame (Granite blit or
    /// fullscreen Canvas draw) for the FPS overlay.
    func noteVizFrame() { vizFrameCount &+= 1 }
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
    /// Bumped on every ID3-editor open request. The editor reloads its file on
    /// each change and the window manager raises the (single) editor window, so
    /// picking a different file while the editor is already open updates it and
    /// brings it to the front instead of doing nothing.
    @Published var id3Request: Int = 0
    /// Artwork image currently shown in the ID3 editor (shared with the artwork zoom window).
    @Published var artworkImage: NSImage? = nil
    /// When true, the artwork zoom window is open.
    @Published var artworkWindowVisible: Bool = false
    /// When true, the artwork window (A6) tracks `nowPlaying.artworkPath` live
    /// instead of showing a fixed image. Set by `openArtworkWindow()` (the
    /// `k` key / A1 art tap); left false by the ID3 editor's zoom tap and the
    /// Media Library's "View Art", which show one specific track's art.
    @Published var artworkFollowsPlayback: Bool = false
    /// Bumped every time the artwork window (A6) should open-or-focus.
    /// Mirrors `id3Request`: `WindowManagerModifier` calls `openWindow` on
    /// every change (not gated on a visibility toggle), so a repeat `k`
    /// press re-fronts the already-open singleton instead of doing nothing.
    @Published var artworkWindowRequest: Int = 0

    /// A1 now-playing panel: expanded (art + auto-cycling tag/tech/stats/
    /// links carousel) vs collapsed (today's compact marquee-only layout).
    /// Persisted the same way as the other window-visibility bools
    /// (`playlistVisible` et al.) — plain UserDefaults, restored in `init()`,
    /// written in `saveState()` and on toggle.
    @Published var playerExpanded: Bool = false
    /// A1 panel data for the current track, rebuilt by `refreshNowPlaying()`
    /// on every track-change call site (tick()'s index-change branch,
    /// `refreshAll()`, `refreshDirtyPlaylistItems()`'s changed branch — all
    /// funnel through `refreshCurrentTrackInfo()`). `nil` when nothing is
    /// playing. There is no FFI push callback for this (unlike GTK's
    /// subscriber seam) — mac polls `sparkamp_now_playing_open` instead.
    @Published var nowPlaying: NowPlayingInfo? = nil

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
    /// BSD names of devices with an eject in flight — drives the "Ejecting…"
    /// spinner and disables the button until DiskArbitration finishes.
    @Published var ejectingDevices: Set<String> = []
    /// Set when an eject fails (device busy); shown as an alert, then cleared.
    @Published var ejectError: String? = nil
    /// Audio files on the device currently shown in the detail view, with their
    /// "synced from" library path. Loaded by `loadDeviceTracks`.
    @Published var deviceTracks: [DeviceTrack] = []
    /// Playlists on the device currently shown in the detail view.
    @Published var devicePlaylists: [DevicePlaylist] = []
    /// True while a copy/sync/scan is running for the selected device — disables
    /// the detail-view actions and shows their busy state.
    @Published var deviceBusy: Bool = false
    /// One-line result of the last device op ("Copied 5 · skipped 1", etc.).
    @Published var deviceStatus: String? = nil
    /// Live copy progress (done/total · filename) while a copy runs; nil when idle.
    @Published var copyProgress: CopyProgress? = nil
    /// A two-way sync whose plan came back with both-changed conflicts, waiting
    /// on the user. Non-nil presents the conflict-resolution sheet; the device
    /// it targets is kept alongside so the resolved choices apply to it.
    @Published var pendingSyncPlan: SyncPlan? = nil
    @Published var pendingSyncDevice: Device? = nil
    /// iOS/PTP devices recognized via ImageCaptureCore (never mount under
    /// /Volumes). They carry `backend == .unsupported` and are shown with a
    /// "can't sync music" banner. Kept separate from `devices` (which the 2 s
    /// volume poll rebuilds) and merged for the UI via `allDevices`.
    @Published var unsupportedDevices: [Device] = []
    /// All devices shown under the Devices group: mounted volumes + the
    /// IC-recognized iOS/PTP devices.
    var allDevices: [Device] { devices + unsupportedDevices }
    /// ImageCaptureCore watcher; created on first use, started while the Media
    /// Library window is open. Publishes into `unsupportedDevices` on the main
    /// thread.
    lazy var unsupportedWatcher: UnsupportedDeviceWatcher = {
        let w = UnsupportedDeviceWatcher()
        w.onChange = { [weak self] list in self?.unsupportedDevices = list }
        return w
    }()
    /// Ticks counted only while the ML window is open; gates the 2 s device poll.
    var deviceTickCount: Int = 0
    /// Always-incrementing tick counter for the optical-drive poll. Unlike the
    /// device poll it runs from app start (independent of the ML window) so an
    /// inserted audio CD can auto-open the library when Sparkamp is the default
    /// CD handler.
    var discPollTickCount: Int = 0

    // ── Optical discs ────────────────────────────────────────────────────────
    /// Every optical drive with its loaded-media state (audio-CD TOC included).
    /// Polled ~every 10 s from app start (subprocess-backed).
    @Published var discDrives: [OpticalDrive] = []
    /// Set by the poll when a drive transitions to "audio CD loaded" and the
    /// auto-open setting is on; the Media Library view consumes it to navigate
    /// to that drive, then clears it back to nil.
    @Published var requestedDiscNav: String? = nil
    /// Tracks of the disc shown in the drive detail view.
    @Published var discTracks: [DiscTrackEntry] = []
    /// True while a disc drive enumeration/track load runs in the background.
    @Published var discBusy: Bool = false
    /// Drive ids with an eject in flight.
    @Published var ejectingDiscs: Set<String> = []
    /// One-line result of the last disc op ("Added 8 disc tracks", …).
    @Published var discStatus: String? = nil
    /// Set when the drive being viewed disconnects mid-session; shown as a
    /// banner on the Disc Drives overview until dismissed or a drive is reopened.
    @Published var discDisconnectNotice: String? = nil
    /// gnudb matches awaiting the user's pick (sheet shown while non-nil;
    /// empty array = "no match" handled inline, never presented).
    @Published var discMatches: [DiscMatch]? = nil
    /// Which drive the pending matches belong to. Lookups keep running when
    /// the window closes or the user navigates away; the sheet re-presents
    /// only on that drive's view, never on an unrelated one.
    @Published var discMatchesDriveId: String? = nil
    /// True while a gnudb query/read runs in the background.
    @Published var discIdentifying: Bool = false
    /// Per-disc tag sets keyed by freedb disc ID — from a gnudb match and/or
    /// hand edits. Overlaid onto `discTracks` titles and consumed by rip
    /// (Phase 3) and submission (Phase 4).
    @Published var discTagSets: [String: DiscTagSet] = [:]
    /// The untouched gnudb match per disc — baseline for "worth submitting?"
    /// and the source of the revision an update must increment.
    @Published var discOfficial: [String: XmcdEntry] = [:]
    /// True while a submission is in flight.
    @Published var discSubmitting: Bool = false
    /// Live rip progress (done/total · current title); nil when idle.
    @Published var ripProgress: CopyProgress? = nil
    /// 0–1 within the track currently encoding (from the core job poll), so
    /// the progress bar moves during a single track too.
    @Published var ripTrackFrac: Double = 0
    /// Set to stop the rip after the track currently encoding.
    var ripCancelRequested: Bool = false
    /// Per-drive burn queues — a dedicated queue separate from the active
    /// playlist (Winamp-style), keyed by drive id so "Send to ▸ Disc Drive
    /// → B" only ever touches B's own queue (mirrors the core's
    /// `disc::burnlist::BurnQueues`). Always read/write through
    /// `burnQueue(for:)` / `addToBurnList(driveId:...)` — never index this
    /// dict directly, so a drive with nothing queued yet reads as empty
    /// rather than crashing. Fed from any "Send to ▸ Disc Drive" action.
    @Published var burnQueues: [String: [BurnEntry]] = [:]
    /// Phase text while a burn runs ("Preparing 2/5 …", "Burning…"); nil idle.
    /// Mirrored from the core burn job's poll; cancel goes through
    /// `cancelBurn()` (the job stops between steps / kills the subprocess).
    @Published var burnPhase: String? = nil
    /// 0–1 progress within `burnPhase` when the core job knows one (streamed
    /// cdrskin percent on Linux; nil during erase/xorriso/drutil phases,
    /// which report no percent — shown as an indeterminate spinner instead).
    @Published var burnFraction: Double? = nil
    /// Per-drive disc-artist/disc-album overrides the user typed on the burn
    /// panel; a drive with no entry here uses freshly computed defaults
    /// (`burnMeta(for:)`) — mirrors core `BurnList.meta_override` (`None` =
    /// recompute). Cleared alongside the queue by `clearBurnList`.
    @Published var burnMetaOverrides: [String: DiscMeta] = [:]
    /// Audio files on the data disc shown in the open drive detail view
    /// (empty when the drive holds no browsable data disc, or the disc
    /// hasn't been read yet).
    @Published var discFiles: [DiscFile] = []
    /// True while a data-disc mount+walk is in flight.
    @Published var discFilesBusy: Bool = false
    /// Set true when loadDiscFiles is called while busy; triggers a deferred
    /// reload once the in-flight load completes. Prevents stale mount paths
    /// when fast unmount/remount occurs during load.
    private var discFilesPendingReload: Bool = false
    /// Paths that failed the duration probe on the most recent "Send to ▸
    /// Disc Drive" — non-nil presents a one-shot alert listing them, then
    /// cleared. Those files are never queued (an unknown duration would
    /// defeat the over-capacity gate).
    @Published var burnUnreadableFiles: [String]? = nil

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
        playerExpanded       = UserDefaults.standard.bool(forKey: "sparkamp.playerExpanded")
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

        // Sync lightweight state that changes during playback. Publish only
        // actual changes: every @Published write fires objectWillChange and
        // invalidates every observing view, changed or not.
        let state = sparkamp_get_state(ctx)
        let playing = (state == 1)
        let paused = (state == 2)
        if isPlaying != playing { isPlaying = playing }
        if isPaused != paused { isPaused = paused }
        let pos = sparkamp_get_position(ctx)
        let dur = sparkamp_get_duration(ctx)
        // While the fullscreen visualizer owns the screen, keep the 10 Hz
        // position/duration stream out of the publisher entirely: nothing
        // visible reads it (the time display lives in the occluded player
        // window), and its per-tick SwiftUI invalidation bursts were
        // stealing slots from the visualizer's strict 30 Hz timer (the
        // observed 20–30 fps wobble). Values keep flowing to the logic
        // below via the locals; publishing resumes on exit.
        if !fullscreenVizVisible {
            position = pos
            duration = dur
        }
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
        if isPlaying, idx >= 0, pos >= playCountThresholdSecs {
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

        // Poll optical drives ~every 10 s from app start, regardless of the ML
        // window. Detection shells out to drutil on a background queue, so it's
        // polled an order of magnitude slower than volumes. Running unconditionally
        // lets an inserted audio CD auto-open the library (default-handler flow);
        // the first tick polls immediately so a disc present at launch is seen.
        discPollTickCount += 1
        if discPollTickCount % 100 == 1 {
            pollDiscDrives()
        }

        // Poll connected USB/volume devices ~every 2 s while the ML window is
        // open. The tick fires at 10 Hz, so every 20th tick. Detection only —
        // counts are computed on demand for the overview (refreshDeviceCounts).
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

        // Position: update seek bar and duration display. Muted while the
        // fullscreen visualizer is up — same rationale as in tick(): the
        // seek bar is occluded, and these @Published writes were part of
        // the invalidation bursts starving the visualizer's frame timer.
        sparkamp_set_position_callback(ctx, { userdata, pos, dur in
            guard let userdata = userdata else { return }
            let model = Unmanaged<SparkampModel>.fromOpaque(userdata).takeUnretainedValue()
            if !model.fullscreenVizVisible {
                model.position = pos
                model.duration = dur
            }
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
        // A1 now-playing panel data — refreshed alongside title/artist so it
        // stays in lockstep with every track-change call site that already
        // routes through here (tick()'s index-change branch, refreshAll(),
        // refreshDirtyPlaylistItems()'s changed branch).
        refreshNowPlaying()
    }

    /// Rebuild `nowPlaying` from the core's now-playing snapshot for the
    /// current track (`sparkamp_now_playing_open` + getters). `nil` when
    /// nothing is playing. Mac has no push callback for this (unlike GTK's
    /// `subscribe_now_playing`); polling here on every track change is the
    /// documented substitute (see sparkamp_bridge.h's Now Playing section).
    func refreshNowPlaying() {
        guard let ctx = ctx, let np = sparkamp_now_playing_open(ctx) else {
            nowPlaying = nil
            loadFollowedArtwork()
            return
        }
        defer { sparkamp_now_playing_close(np) }

        let tagCount = Int(sparkamp_now_playing_tag_count(np))
        var tags: [(String, String)] = []
        tags.reserveCapacity(tagCount)
        for i in 0..<tagCount {
            let labelPtr = sparkamp_now_playing_tag_label(np, Int32(i))
            let valuePtr = sparkamp_now_playing_tag_value(np, Int32(i))
            let label = labelPtr.map { String(cString: $0) } ?? ""
            let value = valuePtr.map { String(cString: $0) } ?? ""
            sparkamp_free_string(labelPtr)
            sparkamp_free_string(valuePtr)
            if !label.isEmpty { tags.append((label, value)) }
        }

        let techPtr = sparkamp_now_playing_tech_line(np)
        let techLine = techPtr.map { String(cString: $0) } ?? ""
        sparkamp_free_string(techPtr)

        let artPtr = sparkamp_now_playing_artwork_path(np)
        let artworkPath = artPtr.map { String(cString: $0) } ?? ""
        sparkamp_free_string(artPtr)

        let hasPlayCount = sparkamp_now_playing_has_play_count(np) != 0
        let playCount = sparkamp_now_playing_play_count(np)

        let lastPlayedPtr = sparkamp_now_playing_last_played(np)
        let lastPlayed = lastPlayedPtr.map { String(cString: $0) } ?? ""
        sparkamp_free_string(lastPlayedPtr)

        let artistWikiPtr = sparkamp_now_playing_artist_wiki_url(np)
        let artistWikiURL = artistWikiPtr.map { String(cString: $0) } ?? ""
        sparkamp_free_string(artistWikiPtr)

        let albumWikiPtr = sparkamp_now_playing_album_wiki_url(np)
        let albumWikiURL = albumWikiPtr.map { String(cString: $0) } ?? ""
        sparkamp_free_string(albumWikiPtr)

        nowPlaying = NowPlayingInfo(
            tags: tags,
            techLine: techLine,
            artworkPath: artworkPath,
            hasPlayCount: hasPlayCount,
            playCount: playCount,
            lastPlayed: lastPlayed,
            artistWikiURL: artistWikiURL,
            albumWikiURL: albumWikiURL
        )
        // Keep the A6 art window in sync if it's following playback.
        loadFollowedArtwork()
    }

}
