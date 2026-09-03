import SwiftUI

// ContentView is the root view. It hosts the player and two alert layers:
//   1. Fatal alert: the core could not start at all
//   2. Playback alert: a runtime playback error (dismissable)
struct ContentView: View {
    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager

    var body: some View {
        // Skin colour scheme + body font are applied at the WindowGroup root
        // via `themedRoot(_:)` in SparkampMacApp.swift, so this view focuses
        // purely on player content + alert layers.
        PlayerWindow()
            // ── Fatal: the core could not start ─────────────────────────────
            // There is nothing for the user to install any more. Audio is
            // AVFoundation and discs are DiscRecording, both part of macOS, so
            // this alert says what failed and stops there rather than offering
            // a Homebrew command that would fix nothing.
            .alert("Sparkamp could not start", isPresented: .constant(model.fatalError != nil)) {
                Button("OK") { model.fatalError = nil }
            } message: {
                Text(model.fatalError ?? "")
            }
            // ── Playback error: dismiss and continue ────────────────────────
            .alert("Playback Error", isPresented: .constant(model.playbackError != nil)) {
                Button("OK") { model.playbackError = nil }
            } message: {
                Text(model.playbackError ?? "")
            }
    }
}
