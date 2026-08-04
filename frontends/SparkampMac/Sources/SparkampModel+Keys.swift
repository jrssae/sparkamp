import SwiftUI
import AppKit

// MARK: - Keyboard shortcuts (app-wide key monitor routing)

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
        guard !hasModifiers, let chars = chars else { return false }

        // Keys that open auxiliary windows or panels are disabled while the
        // fullscreen visualizer is up: the new window appears in the main
        // Space and macOS yanks focus out of fullscreen to show it. (`j`
        // instead exits fullscreen first — see its case below.) `n`/`N` are
        // here for the same reason: an NSOpenPanel pulls focus just as hard
        // as a window does.
        if fullscreenVizVisible, ["p", "i", "u", "d", "k", "m", "n", "N"].contains(chars) {
            return true
        }

        switch chars {
        case "z": prev();          return true
        case "x": play();          return true
        case "c": togglePlay();    return true
        case "v": stop();          return true
        case "V": stopWithFadeout(); return true   // Shift+V
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
        case "q": openJumpQueue(queueMode: true);  return true
        case "m":
            // Toggle the Media Library window (mirror the ML mode button).
            if mediaLibraryVisible { mediaLibraryVisible = false } else { openMediaLibrary() }
            return true
        case "t": toggleStopAfterCurrent();   return true
        case "n": openFilePicker();           return true
        case "N": openFolderPicker();         return true
        case "a": cycleVizMode();             return true
        case "e": graniteRandomEffect();      return true
        case "f": openFullscreenViz();        return true  // toggles open/close
        case "j":
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
                return true
            }
            openJumpQueue(queueMode: false)
            return true
        case "g":
            if fullscreenVizVisible {
                fullscreenFpsVisible.toggle()
                return true
            }
            return false
        case "u": equalizerVisible.toggle(); saveState(); return true
        case "d":
            openId3Editor()  // current track
            return true
        case "w":
            playerExpanded.toggle()
            UserDefaults.standard.set(playerExpanded, forKey: "sparkamp.playerExpanded")
            return true
        case "l":
            openLyricsForFocusedSurface()
            return true
        case "k":
            // Toggle: if the album-art window is the focused (key) window,
            // pressing k again dismisses it; otherwise open-or-focus it (the
            // app-wide key monitor still fires while A6 has focus, so without
            // this a second k was a no-op). Esc / red button still close it too.
            if let w = NSApp.keyWindow, w.title == "Artwork" {
                w.close()
            } else {
                openArtworkWindow()  // A6 — open-or-focus, follows the current track
            }
            return true
        case "\u{1B}":  // Escape — close fullscreen if open
            if fullscreenVizVisible { closeFullscreenViz(); return true }
            return false
        default: break
        }

        // Arrow keys — left/right seek ±5 s, up/down adjust volume unless a
        // track list has focus, in which case ↑/↓ belong to it for row browse.
        switch keyCode {
        case 123: seek(to: ((position - 5) / max(duration, 1)).clamped(to: 0...1)); return true
        case 124: seek(to: ((position + 5) / max(duration, 1)).clamped(to: 0...1)); return true
        case 125:
            if listFocused { return false }
            adjustVolume(by: -0.05)
            return true  // down arrow
        case 126:
            if listFocused { return false }
            adjustVolume(by:  0.05)
            return true  // up arrow
        default: break
        }

        return false
    }
}
