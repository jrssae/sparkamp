import SwiftUI
import AppKit

// MARK: - Settings window

struct SettingsView: View {
    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager

    // Always opens on About — the first tab on every frontend.
    @State private var selectedTab: SettingsTab = .about
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        HStack(spacing: 0) {
            // ── Sidebar ───────────────────────────────────────────────────────
            List(SettingsTab.allCases, id: \.self, selection: $selectedTab) { tab in
                Label(tab.label, systemImage: tab.icon)
                    .tag(tab)
            }
            .listStyle(.sidebar)
            .frame(width: 160)

            Divider()

            // ── Content area ──────────────────────────────────────────────────
            Group {
                switch selectedTab {
                case .about:        AboutPane()
                case .appearance:   AppearancePane()
                case .behavior:     BehaviorPane()
                case .visualizer:   VisualizerPane()
                case .mediaLibrary: MediaLibraryPane()
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        }
        .frame(minWidth: 540, minHeight: 380)
        .preferredColorScheme(themeManager.preferredColorScheme)
        .onDisappear {
            // Sync model flag when window is closed via the system X button.
            model.settingsVisible = false
        }
    }
}

// MARK: - Tab definition

/// Same tabs, same names, same order as GTK's settings notebook — About,
/// Appearance, Behavior, Visualizer, Media Library — and the window always
/// opens on About. GTK's separate "Filetypes" tab held one dropdown (playlist
/// format); that setting lives in Behavior on both frontends now and the tab
/// is gone.
private enum SettingsTab: String, CaseIterable {
    case about, appearance, behavior, visualizer, mediaLibrary

    var label: String {
        switch self {
        case .about:         return "About"
        case .appearance:    return "Appearance"
        case .behavior:      return "Behavior"
        case .visualizer:    return "Visualizer"
        case .mediaLibrary:  return "Media Library"
        }
    }

    var icon: String {
        switch self {
        case .about:         return "info.circle"
        case .appearance:    return "paintbrush"
        case .behavior:      return "slider.horizontal.3"
        case .visualizer:    return "waveform"
        case .mediaLibrary:  return "music.note.house"
        }
    }
}
