import SwiftUI
import AppKit

// Phase 12 F15 — read-only lyrics viewer window. Content comes from
// `SparkampModel.viewOrSearchLyrics` (which called the Rust core); this view
// only lays it out in the skin's body font. Mirrors the singleton-window idiom
// of the ID3 editor: a `@Published lyricsVisible` bool + a `lyricsRequest`
// bump drive open/raise/dismiss from PlayerWindow (see its onChange hooks).
struct LyricsView: View {
    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager

    private var theme: SkinTheme { themeManager.currentTheme }

    var body: some View {
        // No in-window title row: `navigationTitle` below puts the same string
        // in the real titlebar, which is where GTK shows it, and printing it
        // twice just cost a line of body height.
        VStack(alignment: .leading, spacing: 0) {
            ScrollView {
                // Empty body → "No lyrics available" (F15 revision, point 2 —
                // the window still opens).
                Text(model.lyricsText.isEmpty ? "No lyrics available" : model.lyricsText)
                    .font(theme.vars.bodyFont)
                    .foregroundStyle(theme.playlistText)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(12)
            }
            .background(theme.lcdBackground)

            Divider().background(theme.windowBorder)

            HStack(spacing: 10) {
                // This-song vs Now-playing (F15 revision, point 4). Switching to
                // Now-playing retargets onto the currently playing track.
                Picker("", selection: $model.lyricsMode) {
                    Text("This song").tag(LyricsMode.specific)
                    Text("Now playing").tag(LyricsMode.current)
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .fixedSize()
                .onChange(of: model.lyricsMode) { _, m in
                    if m == .current { model.refreshCurrentLyricsIfNeeded() }
                }

                // DuckDuckGo search for the shown track (F15 revision, point 6).
                Button("Search") {
                    guard let url = URL(string: model.lyricsSearchURL) else { return }
                    NSWorkspace.shared.open(url)
                }
                .disabled(model.lyricsSearchURL.isEmpty)

                // Jump to the full ID3 editor for this track (user decision).
                // Force the Lyric row visible there even when the user's field
                // layout hides it — arriving from here, editing the lyrics is
                // the whole point. GTK does the same (F15 revision, point 2).
                Button("Edit in tag editor") {
                    let path = model.lyricsEditPath
                    guard !path.isEmpty else { return }
                    model.lyricsVisible = false
                    model.mlOpenTagEditorForPath(path, forceField: "USLT")
                }
                .disabled(model.lyricsEditPath.isEmpty)
                Spacer()
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(theme.background)
        }
        // Wide enough that the control row fits without "Edit in tag editor"
        // being clipped to "Edit in tag ed…".
        .frame(minWidth: 470, minHeight: 420)
        .background(theme.background)
        .preferredColorScheme(themeManager.preferredColorScheme)
        // The real titlebar, so the Window menu and Mission Control name the
        // song rather than a generic "Lyrics" (GTK sets the same string).
        .navigationTitle(model.lyricsTitle.isEmpty
                         ? "Lyrics" : "Lyrics — \(model.lyricsTitle)")
        // macOS restores this Window scene at launch if it was open at quit,
        // which puts an empty shell on screen — no title, no body, Search and
        // Edit both dead — because nothing asked for a track. Adopt the
        // playing one instead, and mark the window open so the model's idea
        // of it matches what the user can see.
        .onAppear {
            model.lyricsVisible = true
            if model.lyricsEditPath.isEmpty {
                model.lyricsMode = .current
                model.refreshCurrentLyricsIfNeeded()
            }
        }
        // Current mode: follow the playing track (F15 revision, point 4).
        .onChange(of: model.nowPlayingNonce) { _, _ in model.refreshCurrentLyricsIfNeeded() }
        // Esc closes; PlayerWindow's onChange(lyricsVisible) dismisses the window.
        //
        // There is deliberately no `.onDisappear { lyricsVisible = false }`.
        // SwiftUI tears this view down and rebuilds it on ordinary state
        // churn, and each teardown drove that flag false — which PlayerWindow
        // reads as "close the window" and really did close it. Jumping to a
        // track by double-clicking a playlist row made the whole window
        // vanish. Nothing needs the flag lowered on close: reopening goes
        // through a `lyricsRequest` bump, and the follow-the-track refresh is
        // only ever called by this view, which no longer exists once the
        // window is gone.
        .onExitCommand { model.lyricsVisible = false }
    }
}
