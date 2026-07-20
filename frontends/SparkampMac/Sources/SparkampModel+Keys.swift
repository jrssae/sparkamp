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
            // Bail out entirely when the Jump-to-Track window is visible
            // so its List can consume arrow keys for selection movement
            // (and Return / Escape for play / dismiss) instead of the
            // monitor swallowing arrows as volume adjust.  The check is
            // an instance property read on the main actor — safe here
            // because NSEvent local monitors fire on the main thread.
            let jumpVisible = MainActor.assumeIsolated { self.jumpToTrackVisible }
            if jumpVisible { return event }
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

        // Keys that open auxiliary windows are disabled while the fullscreen
        // visualizer is up: the new window appears in the main Space and
        // macOS yanks focus out of fullscreen to show it. (`j` instead exits
        // fullscreen first — see its case below.)
        if fullscreenVizVisible, ["p", "i", "u", "d", "k"].contains(chars) {
            return true
        }

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
                    self.jumpToTrackVisible = true
                }
                return true
            }
            jumpToTrackVisible.toggle()
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
        case "k":
            openArtworkWindow()  // A6 — open-or-focus, follows the current track
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
