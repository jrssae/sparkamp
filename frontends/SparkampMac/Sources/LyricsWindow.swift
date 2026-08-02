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
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text("Lyrics — \(model.lyricsTitle)")
                    .font(theme.vars.bodyFont.weight(.semibold))
                    .foregroundStyle(theme.playlistText)
                    .lineLimit(1)
                    .truncationMode(.tail)
                Spacer()
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(theme.background)

            Divider().background(theme.windowBorder)

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
                Button("Edit in tag editor") {
                    let path = model.lyricsEditPath
                    guard !path.isEmpty else { return }
                    model.lyricsVisible = false
                    model.mlOpenTagEditorForPath(path)
                }
                .disabled(model.lyricsEditPath.isEmpty)
                Spacer()
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(theme.background)
        }
        .frame(minWidth: 360, minHeight: 420)
        .background(theme.background)
        .preferredColorScheme(themeManager.preferredColorScheme)
        // Current mode: follow the playing track (F15 revision, point 4).
        .onChange(of: model.nowPlayingNonce) { _, _ in model.refreshCurrentLyricsIfNeeded() }
        // Esc closes; PlayerWindow's onChange(lyricsVisible) dismisses the window.
        .onExitCommand { model.lyricsVisible = false }
        .onDisappear { model.lyricsVisible = false }
    }
}
