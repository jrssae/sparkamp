import SwiftUI
import AppKit

// MARK: - Keyboard shortcuts (app-wide key monitor routing)

/// What a key press means, decided apart from doing it so the map can be
/// tested without a running app. `resolve` is the whole decision; the model's
/// `perform` only carries it out.
enum KeyCommand: Equatable {
    case previous, play, togglePlay, stop, stopWithFadeout, next
    case cycleRepeat, toggleShuffle
    case volumeDown, volumeUp
    case seekBack, seekForward
    case togglePlaylist, toggleShortcuts, openQueue, toggleMediaLibrary
    case toggleStopAfterCurrent, addFiles, addFolder
    case cycleVisualizer, randomGraniteEffect, toggleFullscreen
    case openJump, toggleRemainingTime, toggleFpsOverlay
    case toggleEqualizer, editTags, togglePlayerPanel, openLyrics, toggleArtwork
    case exitFullscreen
    /// Consumed and ignored: a key that would open a window over the
    /// fullscreen visualizer.
    case blockedInFullscreen

    /// Keys that open auxiliary windows or panels, disabled while the
    /// fullscreen visualizer is up: the new window appears in the main Space
    /// and macOS yanks focus out of fullscreen to show it. (`j` instead exits
    /// fullscreen first; see `SparkampModel.perform`.) `n`/`N` are here for
    /// the same reason: an NSOpenPanel pulls focus just as hard as a window.
    static let blockedInFullscreenKeys: Set<String> = ["p", "i", "u", "d", "k", "m", "n", "N"]

    static let leftArrow: UInt16 = 123
    static let rightArrow: UInt16 = 124
    static let downArrow: UInt16 = 125
    static let upArrow: UInt16 = 126

    /// The command for a key press, or nil when the key is not Sparkamp's and
    /// should reach whatever has focus.
    ///
    /// `listFocused` reports whether a track list holds focus; it keeps ↑/↓
    /// for browsing rows instead of volume. `fullscreen` reports whether the
    /// fullscreen visualizer is up.
    static func resolve(
        chars: String?,
        keyCode: UInt16,
        hasModifiers: Bool,
        listFocused: Bool,
        fullscreen: Bool
    ) -> KeyCommand? {
        guard !hasModifiers, let chars else { return nil }
        if fullscreen, blockedInFullscreenKeys.contains(chars) {
            return .blockedInFullscreen
        }
        switch chars {
        case "z": return .previous
        case "x": return .play
        case "c": return .togglePlay
        case "v": return .stop
        case "V": return .stopWithFadeout   // Shift+V
        case "b": return .next
        case "r": return .cycleRepeat
        case "s": return .toggleShuffle
        case "-": return .volumeDown
        case "=": return .volumeUp
        case "p": return .togglePlaylist
        case "i": return .toggleShortcuts
        case "q": return .openQueue
        case "m": return .toggleMediaLibrary
        case "t": return .toggleStopAfterCurrent
        case "n": return .addFiles
        case "N": return .addFolder
        case "a": return .cycleVisualizer
        case "e": return .randomGraniteEffect
        case "f": return .toggleFullscreen   // toggles open/close
        case "j": return .openJump
        // Same toggle as clicking the counter, for anyone who would rather
        // not reach for the mouse. GTK and the TUI bind the same key.
        case "h": return .toggleRemainingTime
        case "g": return fullscreen ? .toggleFpsOverlay : nil
        case "u": return .toggleEqualizer
        case "d": return .editTags
        case "w": return .togglePlayerPanel
        case "l": return .openLyrics
        case "k": return .toggleArtwork
        case "\u{1B}": return fullscreen ? .exitFullscreen : nil   // Escape
        default: break
        }
        // Arrow keys: left/right seek ±5 s, up/down adjust volume unless a
        // track list has focus, in which case ↑/↓ belong to it for row browse.
        switch keyCode {
        case leftArrow: return .seekBack
        case rightArrow: return .seekForward
        case downArrow: return listFocused ? nil : .volumeDown
        case upArrow: return listFocused ? nil : .volumeUp
        default: return nil
        }
    }
}

extension SparkampModel {
    // MARK: Keyboard shortcuts

    func startKeyMonitor() {
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            guard let self = self else { return event }
            // Don't intercept keys when a text field has focus — SwiftUI's
            // TextField is backed by NSTextView on macOS, so this one check
            // covers all text inputs (jump-to-track search, etc.).
            if NSApp.keyWindow?.firstResponder is NSTextView { return event }
            // Bail out entirely when the Jump / Queue window is showing its
            // Jump pane, so the search List can consume arrow keys for
            // selection movement (and Return / Escape for play / dismiss)
            // instead of the monitor swallowing arrows as volume adjust —
            // and so plain letters reach the search field as text. Queue mode
            // has no text field, so the monitor stays live there and `q`
            // still closes the window. The check is an instance property read
            // on the main actor — safe here because NSEvent local monitors
            // fire on the main thread.
            let jumpPaneUp = MainActor.assumeIsolated {
                self.jumpToTrackVisible && !self.jumpQueueMode
            }
            if jumpPaneUp { return event }
            let chars   = event.charactersIgnoringModifiers
            let keyCode = event.keyCode
            let hasMods = !event.modifierFlags
                .intersection([.command, .option, .control])
                .isEmpty
            // A focused track list keeps ↑/↓ for row browsing; only the player
            // window turns them into volume. GTK draws the same line (its ↑/↓
            // live in the main-window key controller, not the shared handler),
            // and without this the monitor swallows the arrows before the
            // table ever sees them, so no list could be browsed by keyboard.
            let listFocused = NSApp.keyWindow?.firstResponder is NSTableView
            let consumed = MainActor.assumeIsolated {
                self.handleRawKey(
                    chars: chars,
                    keyCode: keyCode,
                    hasModifiers: hasMods,
                    listFocused: listFocused
                )
            }
            return consumed ? nil : event
        }
    }

    /// Handle a key expressed as plain Sendable values. Returns true if consumed.
    ///
    /// `listFocused` reports whether a track list currently holds focus; it
    /// hands ↑/↓ back to that list instead of treating them as volume.
    @discardableResult
    func handleRawKey(
        chars: String?,
        keyCode: UInt16,
        hasModifiers: Bool,
        listFocused: Bool = false
    ) -> Bool {
        guard let command = KeyCommand.resolve(
            chars: chars,
            keyCode: keyCode,
            hasModifiers: hasModifiers,
            listFocused: listFocused,
            fullscreen: fullscreenVizVisible
        ) else { return false }
        perform(command)
        return true
    }

    /// Carry out a key's command. Every case is consumed; `resolve` already
    /// decided which keys are Sparkamp's.
    private func perform(_ command: KeyCommand) {
        switch command {
        case .previous:         prev()
        case .play:             play()
        case .togglePlay:       togglePlay()
        case .stop:             stop()
        case .stopWithFadeout:  stopWithFadeout()
        case .next:             next()
        case .cycleRepeat:      cycleRepeat()
        case .toggleShuffle:    toggleShuffle()
        case .volumeDown:       adjustVolume(by: -0.05)
        case .volumeUp:         adjustVolume(by:  0.05)
        case .seekBack:
            seek(to: ((position - 5) / max(duration, 1)).clamped(to: 0...1))
        case .seekForward:
            seek(to: ((position + 5) / max(duration, 1)).clamped(to: 0...1))
        case .togglePlaylist:
            playlistVisible.toggle()
            UserDefaults.standard.set(playlistVisible, forKey: "sparkamp.playlistVisible")
        case .toggleShortcuts:  toggleKeyboardShortcuts()
        case .openQueue:        openJumpQueue(queueMode: true)
        case .toggleMediaLibrary:
            // Toggle the Media Library window (mirror the ML mode button).
            if mediaLibraryVisible { mediaLibraryVisible = false } else { openMediaLibrary() }
        case .toggleStopAfterCurrent: toggleStopAfterCurrent()
        case .addFiles:         openFilePicker()
        case .addFolder:        openFolderPicker()
        case .cycleVisualizer:  cycleVizMode()
        case .randomGraniteEffect: graniteRandomEffect()
        case .toggleFullscreen: openFullscreenViz()
        case .openJump:
            if fullscreenVizVisible {
                // Leave fullscreen first, then open the jump window once
                // back in the main Space. Opening it over fullscreen makes
                // macOS switch Spaces and fight for focus. 0.8 s clears the
                // 0.7 s fullscreen-exit animation in closeFullscreenViz.
                closeFullscreenViz()
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.8) {
                    self.jumpQueueMode = false
                    self.jumpToTrackVisible = true
                }
            } else {
                openJumpQueue(queueMode: false)
            }
        case .toggleRemainingTime: toggleRemainingTime()
        case .toggleFpsOverlay: fullscreenFpsVisible.toggle()
        case .toggleEqualizer:  equalizerVisible.toggle(); saveState()
        case .editTags:         openId3Editor()  // current track
        case .togglePlayerPanel:
            playerExpanded.toggle()
            UserDefaults.standard.set(playerExpanded, forKey: "sparkamp.playerExpanded")
        case .openLyrics:       openLyricsForFocusedSurface()
        case .toggleArtwork:
            // Toggle: if the album-art window is the focused (key) window,
            // pressing k again dismisses it; otherwise open-or-focus it (the
            // app-wide key monitor still fires while A6 has focus, so without
            // this a second k was a no-op). Esc / red button still close it too.
            if let w = NSApp.keyWindow, w.title == "Artwork" {
                w.close()
            } else {
                openArtworkWindow()  // A6 — open-or-focus, follows the current track
            }
        case .exitFullscreen:   closeFullscreenViz()
        case .blockedInFullscreen: break
        }
    }
}
