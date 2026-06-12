import SwiftUI
import AppKit
import IOKit.pwr_mgt

// MARK: - Transport, playlist actions, file picker, persistence

extension SparkampModel {
    // MARK: Transport actions

    func play()  { if let ctx = ctx { sparkamp_play(ctx);  tick(); announceNowPlaying() } }
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
        announceNowPlaying()
    }

    func prev() {
        guard let ctx = ctx else { return }
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
        id3TrackIndex = trackIndex
        id3EditorVisible = true
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

}
