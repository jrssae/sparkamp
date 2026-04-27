import SwiftUI

// ContentView is the root view. It hosts the player and two alert layers:
//   1. Fatal alert  — GStreamer could not be initialised (shows install instructions)
//   2. Playback alert — a runtime GStreamer bus error (dismissable)
struct ContentView: View {
    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager

    var body: some View {
        // Skin colour scheme + body font are applied at the WindowGroup root
        // via `themedRoot(_:)` in SparkampMacApp.swift, so this view focuses
        // purely on player content + alert layers.
        PlayerWindow()
            // ── Fatal: GStreamer not found ──────────────────────────────────
            .alert("GStreamer not found", isPresented: .constant(model.fatalError != nil)) {
                Button("OK") { model.fatalError = nil }
                Button("Copy install command") {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(
                        "brew install gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad gst-plugins-ugly",
                        forType: .string
                    )
                    model.fatalError = nil
                }
            } message: {
                Text(model.fatalError ?? "")
                Text("\nInstall via Homebrew:\nbrew install gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad gst-plugins-ugly")
            }
            // ── Playback error: dismiss and continue ────────────────────────
            .alert("Playback Error", isPresented: .constant(model.playbackError != nil)) {
                Button("OK") { model.playbackError = nil }
            } message: {
                Text(model.playbackError ?? "")
            }
    }
}
