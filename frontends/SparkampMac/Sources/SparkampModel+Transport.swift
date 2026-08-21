import SwiftUI
import AppKit
import IOKit.pwr_mgt

// MARK: - Transport, playlist actions, file picker, persistence

extension SparkampModel {
    // MARK: Transport actions

    func play()  { if let ctx = ctx { sparkamp_play(ctx);  tick(); announceNowPlaying() } }
    func pause() { if let ctx = ctx { sparkamp_pause(ctx); tick() } }
    func stop()  { if let ctx = ctx { setStopAfterCurrent(false); sparkamp_stop(ctx); tick() } }

    func togglePlay() {
        if isPlaying { pause() } else { play() }
    }

    // MARK: Stop with fadeout (Shift+V)

    /// Ramp the output down to silence and then stop, over
    /// `playback.fadeout_secs`. The ramp runs in the engine and is advanced by
    /// `sparkamp_tick`, so this returns straight away and the stop lands a
    /// fade-length later — `tick()` picks the state change up on its own.
    func stopWithFadeout() {
        guard let ctx = ctx else { return }
        setStopAfterCurrent(false)
        sparkamp_stop_with_fadeout(ctx)
        isFadingOut = sparkamp_is_fading_out(ctx)
    }

    /// Stop-with-fadeout length in seconds (Settings).
    func setFadeoutSeconds(_ secs: Int) {
        guard let ctx = ctx else { return }
        sparkamp_set_fadeout_secs(ctx, UInt32(max(0, secs)))
        fadeoutSeconds = Int(sparkamp_get_fadeout_secs(ctx))
        saveState()
    }

    // MARK: Stop after current (phase 6)

    /// Set the engine stop-after-current flag and mirror it to the published
    /// property that drives the play-button badge.
    func setStopAfterCurrent(_ v: Bool) {
        guard let ctx = ctx else { return }
        sparkamp_set_stop_after_current(ctx, v)
        stopAfterCurrent = v
    }

    /// Toggle stop-after-current (key `t`).
    func toggleStopAfterCurrent() {
        guard let ctx = ctx else { return }
        setStopAfterCurrent(!sparkamp_get_stop_after_current(ctx))
    }

    func next() {
        guard let ctx = ctx else { return }
        // Manual skip cancels a pending stop-after-current.
        setStopAfterCurrent(false)
        sparkamp_nav_next(ctx)
        refreshAll()
        saveState()
        announceNowPlaying()
    }

    func prev() {
        guard let ctx = ctx else { return }
        // Manual skip cancels a pending stop-after-current.
        setStopAfterCurrent(false)
        sparkamp_nav_prev(ctx)
        refreshAll()
        saveState()
        announceNowPlaying()
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

    /// Open the combined Jump / Queue window in the requested pane, or close it
    /// if it is already open showing that pane. `j` asks for Jump, `q` for
    /// Queue; pressing the other one while it is open just switches panes, the
    /// same as clicking its radio button.
    func openJumpQueue(queueMode: Bool) {
        if jumpToTrackVisible && jumpQueueMode == queueMode {
            jumpToTrackVisible = false
            return
        }
        jumpQueueMode = queueMode
        jumpToTrackVisible = true
    }

    func cycleVizMode() {
        guard let ctx = ctx else { return }
        sparkamp_cycle_viz_mode(ctx)
        vizMode = Int(sparkamp_get_viz_mode(ctx))
        // Persist immediately: the willTerminate save never runs when the
        // process is killed (Xcode Stop sends SIGKILL), and "which
        // visualizer was I on" is exactly what users expect to survive.
        saveState()
    }

    /// Switch Granite to a random other effect (`e` key). No-op until the
    /// Granite renderer has drawn its first frame.
    func graniteRandomEffect() {
        guard let ctx = ctx else { return }
        _ = sparkamp_granite_random_effect(ctx)
        saveState()
    }

    func openFullscreenViz() {
        if fullscreenVizVisible { closeFullscreenViz(); return }
        guard let ctx = ctx else { return }
        let mode = sparkamp_get_viz_mode(ctx)
        // Fullscreen for Waveform (1) and Granite (2). Bars (0) stays excluded
        // for parity with GTK.
        guard mode == 1 || mode == 2 else { return }
        fullscreenVizVisible = true
    }

    func openId3Editor(trackIndex: Int = -1) {
        id3DirectPath = ""          // playlist-index mode; drop any stale direct path
        id3TrackIndex = trackIndex
        id3EditorVisible = true
        id3Request &+= 1
    }

    /// Hold a no-display-sleep assertion exactly while the fullscreen
    /// visualizer is open AND the keep-awake setting is on. Without it
    /// macOS sleeps the display mid-visualization, and on wake bounces
    /// between the main Space and the fullscreen Space.
    func updateDisplaySleepAssertion() {
        let wantAwake = fullscreenVizVisible
            && (ctx.map { sparkamp_get_keep_screen_awake($0) } ?? false)
        if wantAwake && displaySleepAssertion == 0 {
            var id: IOPMAssertionID = 0
            let result = IOPMAssertionCreateWithName(
                kIOPMAssertionTypePreventUserIdleDisplaySleep as CFString,
                IOPMAssertionLevel(kIOPMAssertionLevelOn),
                "Sparkamp fullscreen visualizer" as CFString,
                &id
            )
            if result == kIOReturnSuccess { displaySleepAssertion = id }
        } else if !wantAwake && displaySleepAssertion != 0 {
            IOPMAssertionRelease(displaySleepAssertion)
            displaySleepAssertion = 0
        }
    }

    /// Settings toggle: persist + apply to any currently-held assertion.
    func setKeepScreenAwake(_ on: Bool) {
        guard let ctx = ctx else { return }
        sparkamp_set_keep_screen_awake(ctx, on)
        saveState()
        updateDisplaySleepAssertion()
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

    /// How much state a track start refreshes. `.everything` is for the paths
    /// that may also have changed the list itself.
    enum TrackStartRefresh { case currentTrack, everything }

    /// Start the track at `index`, doing everything a track change owes the
    /// rest of the app — not just the jump.
    ///
    /// There are seven routes that start a track: `jumpTo`, the two explicit
    /// `replace*` actions, and the four add-then-autoplay paths. Each was
    /// written separately and each learned this list the hard way; an audit on
    /// 2026-08-11 found four of the seven still doing only part of it, with two
    /// reproducible bugs to show for it (see below). The obligations live here
    /// now so a new add path cannot pick up three of the five and look right.
    ///
    /// - `setStopAfterCurrent(false)` — a pending stop-after-current survives a
    ///   playlist change and halts playback after this first track. This is
    ///   what made `addFiles` stop dead in replace+autoplay mode.
    /// - `saveState()` — several callers `clearPlaylist()` first, and *that*
    ///   persists the EMPTY list. Without a save afterwards the new tracks live
    ///   only on screen and are gone on relaunch. This is what lost the disc
    ///   tracks added by `addDiscTracks` in replace mode.
    /// - `announceNowPlaying()` — the nonce-driven observers (the lyrics window
    ///   in Now-playing mode, the fullscreen track toast) otherwise sit on the
    ///   song that was playing before.
    ///
    /// `play` is false only for `jumpTo`, which re-jumps inside a list that is
    /// already playing and lets the engine carry on.
    func startTrack(at index: Int,
                    play: Bool = true,
                    refresh: TrackStartRefresh = .currentTrack) {
        guard let ctx = ctx else { return }
        setStopAfterCurrent(false)
        sparkamp_playlist_jump(ctx, Int32(index))
        if play { sparkamp_play(ctx) }
        switch refresh {
        case .currentTrack: refreshCurrentTrackInfo()
        case .everything:   refreshAll()
        }
        saveState()
        announceNowPlaying()
    }

    func jumpTo(index: Int) {
        // Jumping to / replaying a track clears a pending stop-after-current
        // (pause/resume via togglePlay does not). No `play` — the engine is
        // already running and a jump inside a live list continues it.
        startTrack(at: index, play: false, refresh: .everything)
    }

    // MARK: Playlist actions

    /// Returns the rows the new tracks landed on, so a caller that dropped
    /// them at a position can slide the block there afterwards. Empty when
    /// nothing was added.
    @discardableResult
    func addFiles(_ urls: [URL]) -> [Int] {
        guard let ctx = ctx else { return [] }

        // Core decides. `sparkamp_should_replace_on_add` is the same rule GTK
        // and the TUI use, so the three frontends cannot drift on what
        // "Replace playlist" means. 0 = honour the configured setting.
        let shouldReplace = sparkamp_should_replace_on_add(ctx, 0) == 1
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
                startTrack(at: newIndices[0])
            } else {
                saveState()
            }
        }
        return newIndices
    }

    /// Replace the active playlist with `paths` (files only) and start playing,
    /// regardless of the append/replace setting — the explicit "Replace Current
    /// Playlist" context action on the disc-data and device views. Unlike
    /// `addFiles` (which honors the config setting), this always clears first.
    func replacePlaylistWithPaths(_ paths: [String]) {
        guard let ctx = ctx, !paths.isEmpty else { return }
        sparkamp_playlist_clear(ctx)
        var newIndices: [Int] = []
        for p in paths {
            let idx = p.withCString { sparkamp_playlist_add_fast(ctx, $0) }
            if idx >= 0 { newIndices.append(Int(idx)) }
        }
        refreshPlaylist()
        for i in newIndices {
            sparkamp_scan_metadata(ctx, Int32(i))
            sparkamp_probe_duration(ctx, Int32(i))
        }
        if !newIndices.isEmpty {
            lastAddTime = Date()
            startTrack(at: newIndices[0])
        }
    }

    /// Append `paths` to the active playlist, never replacing it.
    ///
    /// The counterpart to [`replacePlaylistWithPaths`], and the same pair GTK
    /// offers as its Enqueue and Play buttons. Enqueue is an explicit
    /// instruction, so it ignores the add-behavior setting entirely rather
    /// than passing a mode to `sparkamp_should_replace_on_add` — there is no
    /// mode in which it would clear.
    ///
    /// Autoplay follows GTK's Enqueue rule: start playing only when the
    /// playlist was empty beforehand, so queueing more music never interrupts
    /// what is already playing.
    func enqueuePaths(_ paths: [String]) {
        guard let ctx = ctx, !paths.isEmpty else { return }
        let wasEmpty = sparkamp_playlist_len(ctx) == 0
        var newIndices: [Int] = []
        for p in paths {
            let idx = p.withCString { sparkamp_playlist_add_fast(ctx, $0) }
            if idx >= 0 { newIndices.append(Int(idx)) }
        }
        refreshPlaylist()
        for i in newIndices {
            sparkamp_scan_metadata(ctx, Int32(i))
            sparkamp_probe_duration(ctx, Int32(i))
        }
        guard let first = newIndices.first else { return }
        lastAddTime = Date()
        if sparkamp_get_autoplay_on_add(ctx) && wasEmpty {
            startTrack(at: first)
        } else {
            saveState()
        }
    }

    func removeTrack(at index: Int) {
        guard let ctx = ctx else { return }
        sparkamp_playlist_remove(ctx, Int32(index))
        refreshPlaylist()
        saveState()
    }

    /// Move every row in `from` to `to` as one block, keeping their relative
    /// order. Returns where the block landed so the caller can re-select it.
    ///
    /// This used to take the same `IndexSet` and move only `from.first`. A
    /// multi-row drag therefore left every other selected row behind while the
    /// rows between them shifted around the one that did move — which looked
    /// like rows moving at random. Core does the whole block in one pass now
    /// (`Playlist::move_tracks`, ported from GTK's reorder), because replaying
    /// a single move per row walks them into the wrong places: each one shifts
    /// every index after it.
    @discardableResult
    func moveTracks(from: IndexSet, to: Int) -> Int? {
        guard let ctx = ctx, !from.isEmpty else { return nil }
        let rows = from.map { Int32($0) }
        let landed = rows.withUnsafeBufferPointer { buf in
            sparkamp_playlist_move_many(ctx, buf.baseAddress, Int32(buf.count), Int32(to))
        }
        guard landed >= 0 else { return nil }
        refreshPlaylist()
        saveState()
        return Int(landed)
    }

    func clearPlaylist() {
        guard let ctx = ctx else { return }
        sparkamp_playlist_clear(ctx)
        refreshPlaylist()
        saveState()
    }

    // MARK: Playlist reorder (phase 7)

    enum PlaylistSortKey: Int32 { case title = 0, artist = 1, album = 2, filename = 3, path = 4 }

    func sortPlaylist(_ key: PlaylistSortKey) {
        guard let ctx = ctx else { return }
        sparkamp_playlist_sort(ctx, key.rawValue)
        refreshAll(); saveState()
    }

    func reversePlaylist() {
        guard let ctx = ctx else { return }
        sparkamp_playlist_reverse(ctx); refreshAll(); saveState()
    }

    func randomizePlaylist() {
        guard let ctx = ctx else { return }
        sparkamp_playlist_randomize(ctx); refreshAll(); saveState()
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
        UserDefaults.standard.set(playerExpanded,      forKey: "sparkamp.playerExpanded")
    }

}
