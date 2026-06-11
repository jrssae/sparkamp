import SwiftUI

// MARK: - Keyboard shortcuts window

/// Displays all keyboard shortcuts in the same visual style as the playlist.
/// Opened via the ℹ button in the player info panel or by pressing `i`.
struct KeyboardShortcutsView: View {
    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager

    private var theme: SkinTheme { themeManager.currentTheme }

    // MARK: – Shortcut table definition
    // Mirrors the Linux Sparkamp keybinding set exactly.

    private struct ShortcutEntry: Identifiable {
        let id = UUID()
        let key: String
        let action: String
    }

    private let sections: [(title: String, entries: [ShortcutEntry])] = [
        (
            title: "Playback",
            entries: [
                ShortcutEntry(key: "x",          action: "Play"),
                ShortcutEntry(key: "c",          action: "Play / Pause"),
                ShortcutEntry(key: "v",          action: "Stop"),
                ShortcutEntry(key: "z",          action: "Previous track"),
                ShortcutEntry(key: "b",          action: "Next track"),
            ]
        ),
        (
            title: "Seeking",
            entries: [
                ShortcutEntry(key: "← / →",     action: "Seek −5 s / +5 s"),
            ]
        ),
        (
            title: "Volume",
            entries: [
                ShortcutEntry(key: "− / =",      action: "Volume −5 % / +5 %"),
                ShortcutEntry(key: "↑ / ↓",      action: "Volume +5 % / −5 %"),
            ]
        ),
        (
            title: "Playlist & modes",
            entries: [
                ShortcutEntry(key: "r",          action: "Cycle repeat (Off → One → All)"),
                ShortcutEntry(key: "s",          action: "Toggle shuffle"),
                ShortcutEntry(key: "p",          action: "Toggle playlist window"),
                ShortcutEntry(key: "i",          action: "Toggle this shortcuts window"),
                ShortcutEntry(key: "j",          action: "Jump to track (search)"),
            ]
        ),
        (
            title: "Visualizer",
            entries: [
                ShortcutEntry(key: "a",          action: "Cycle visualizer mode (Bars / Waveform / Granite)"),
                ShortcutEntry(key: "n",          action: "Random Granite effect (Granite mode)"),
                ShortcutEntry(key: "f",          action: "Fullscreen visualizer (Waveform or Granite mode)"),
                ShortcutEntry(key: "dbl-click",  action: "Fullscreen visualizer (double-click)"),
                ShortcutEntry(key: "Esc",        action: "Exit fullscreen"),
            ]
        ),
        (
            title: "Time display",
            entries: [
                ShortcutEntry(key: "click time", action: "Switch elapsed / remaining"),
            ]
        ),
    ]

    // MARK: – Body

    var body: some View {
        let vars = themeManager.currentVars
        return VStack(spacing: 0) {
            // Header — matches playlist header style
            HStack {
                Text("Keyboard Shortcuts")
                    .font(vars.bodyFont)
                    .foregroundStyle(theme.playlistDurationText)
                Spacer()
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 5)
            .background(theme.playlistBg.opacity(0.7))

            Divider()
                .background(theme.windowBorder)

            ScrollView {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(sections, id: \.title) { section in
                        // Section header
                        Text(section.title.uppercased())
                            .font(vars.bodyFont.weight(.semibold))
                            .foregroundStyle(theme.playlistDurationText)
                            .padding(.horizontal, 10)
                            .padding(.top, 10)
                            .padding(.bottom, 3)

                        // Rows
                        ForEach(section.entries) { entry in
                            HStack(spacing: 0) {
                                // Key badge
                                Text(entry.key)
                                    .font(vars.smallMonospaceFont)
                                    .foregroundStyle(theme.playlistCurrentText)
                                    .frame(width: 100, alignment: .leading)
                                    .padding(.leading, 10)

                                // Action description — same font as playlist rows
                                Text(entry.action)
                                    .font(vars.bodyFont)
                                    .foregroundStyle(theme.playlistText)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                    .padding(.trailing, 10)
                            }
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, 3)
                        }
                    }

                    Spacer(minLength: 10)
                }
            }
            .background(theme.playlistBg)
        }
        .background(theme.playlistBg)
        .preferredColorScheme(themeManager.preferredColorScheme)
        .onDisappear {
            model.keyboardShortcutsVisible = false
        }
    }
}
