import SwiftUI
import AppKit

// MARK: - Settings window

struct SettingsView: View {
    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager

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
                case .about:       AboutPane()
                case .appearance:  AppearancePane()
                case .playback:    PlaybackPane()
                case .visualizer:  VisualizerPane()
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

private enum SettingsTab: String, CaseIterable {
    case about, appearance, playback, visualizer

    var label: String {
        switch self {
        case .about:       return "About"
        case .appearance:  return "Appearance"
        case .playback:    return "Playback"
        case .visualizer:  return "Visualizer"
        }
    }

    var icon: String {
        switch self {
        case .about:       return "info.circle"
        case .appearance:  return "paintbrush"
        case .playback:    return "play.circle"
        case .visualizer:  return "waveform"
        }
    }
}

// MARK: - About pane

private struct AboutPane: View {
    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack(spacing: 16) {
                Image(nsImage: NSApp.applicationIconImage)
                    .resizable()
                    .frame(width: 64, height: 64)

                VStack(alignment: .leading, spacing: 4) {
                    Text("Sparkamp")
                        .font(.title2.bold())
                    Text("Version 0.3.0")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                    Text("Open source Winamp-style audio player")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }
            }

            Divider()

            VStack(alignment: .leading, spacing: 6) {
                Text("Engine")
                    .font(.headline)
                Text("GStreamer — playbin, equalizer-10bands, volume")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            }

            VStack(alignment: .leading, spacing: 6) {
                Text("License")
                    .font(.headline)
                Link("GNU Affero General Public License v3 (AGPL-3.0)",
                     destination: URL(string: "https://www.gnu.org/licenses/agpl-3.0.html")!)
                    .font(.subheadline)
            }

            Spacer()
        }
        .padding(24)
    }
}

// MARK: - Appearance pane

private struct AppearancePane: View {
    @EnvironmentObject var themeManager: ThemeManager
    @Environment(\.colorScheme) private var colorScheme

    @State private var themeChoice: Int = 0   // 0=System, 1=Dark, 2=Light

    var body: some View {
        Form {
            Section("Theme") {
                Picker("Color scheme", selection: $themeChoice) {
                    Text("System").tag(0)
                    Text("Dark").tag(1)
                    Text("Light").tag(2)
                }
                .pickerStyle(.segmented)
                .onChange(of: themeChoice) { _, newValue in
                    switch newValue {
                    case 1: themeManager.useDark()
                    case 2: themeManager.useLight()
                    default: themeManager.useSystem(colorScheme: colorScheme)
                    }
                }

                Button("Load Custom Skin…") {
                    themeManager.openSkinPicker(colorScheme: colorScheme)
                }

                if case .custom(let url) = themeManager.themeSource {
                    HStack {
                        Text(url.lastPathComponent)
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                        Spacer()
                        Button("Remove", role: .destructive) {
                            themeManager.removeCustomSkin(colorScheme: colorScheme)
                        }
                        .buttonStyle(.borderless)
                    }
                }
            }
        }
        .formStyle(.grouped)
        .onAppear {
            switch themeManager.themeSource {
            case .dark:   themeChoice = 1
            case .light:  themeChoice = 2
            default:      themeChoice = 0
            }
        }
    }
}

// MARK: - Playback pane

private struct PlaybackPane: View {
    @EnvironmentObject var model: SparkampModel

    @State private var autoplayOnAdd: Bool = false
    @State private var addBehavior: Int    = 0    // 0=Append, 1=Replace

    var body: some View {
        Form {
            Section("On Add") {
                Toggle("Autoplay when files are added", isOn: $autoplayOnAdd)
                    .onChange(of: autoplayOnAdd) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_autoplay_on_add(ctx, newValue)
                        sparkamp_save_config(ctx)
                    }

                Picker("When adding files", selection: $addBehavior) {
                    Text("Append to playlist").tag(0)
                    Text("Replace playlist").tag(1)
                }
                .pickerStyle(.radioGroup)
                .onChange(of: addBehavior) { _, newValue in
                    guard let ctx = model.ctx else { return }
                    sparkamp_set_playlist_add_behavior(ctx, Int32(newValue))
                    sparkamp_save_config(ctx)
                }
            }
        }
        .formStyle(.grouped)
        .onAppear {
            guard let ctx = model.ctx else { return }
            autoplayOnAdd = sparkamp_get_autoplay_on_add(ctx)
            addBehavior   = Int(sparkamp_get_playlist_add_behavior(ctx))
        }
    }
}

// MARK: - Visualizer pane

private struct VisualizerPane: View {
    @EnvironmentObject var model: SparkampModel

    @State private var vizMode: Int          = 0     // 0=Bars, 1=Waveform
    @State private var barsMirror: Bool      = true
    @State private var barsZones: Int        = 3
    @State private var barsZoneColors: [Color]     = Array(repeating: .green, count: 6)
    @State private var waveformStyle: Int    = 0     // 0=Lines, 1=Filled
    @State private var waveformZones: Int    = 3
    @State private var waveformZoneColors: [Color] = Array(repeating: .green, count: 6)

    var body: some View {
        Form {
            Section("Mode") {
                Picker("Visualizer mode", selection: $vizMode) {
                    Text("Bars").tag(0)
                    Text("Waveform").tag(1)
                }
                .pickerStyle(.segmented)
                .onChange(of: vizMode) { _, newValue in
                    guard let ctx = model.ctx else { return }
                    sparkamp_set_viz_mode(ctx, Int32(newValue))
                    sparkamp_save_config(ctx)
                }
            }

            if vizMode == 0 {
                Section("Bars") {
                    Toggle("Mirror (extend above and below center)", isOn: $barsMirror)
                        .onChange(of: barsMirror) { _, newValue in
                            guard let ctx = model.ctx else { return }
                            sparkamp_set_viz_mirror(ctx, newValue)
                            sparkamp_save_config(ctx)
                        }

                    Stepper("Zones: \(barsZones)", value: $barsZones, in: 1...6)
                        .onChange(of: barsZones) { _, newValue in
                            guard let ctx = model.ctx else { return }
                            sparkamp_set_viz_zones(ctx, Int32(newValue))
                            sparkamp_save_config(ctx)
                        }

                    ForEach(0..<barsZones, id: \.self) { i in
                        HStack {
                            Text("Zone \(i + 1) color")
                            Spacer()
                            ColorPicker("", selection: $barsZoneColors[i])
                                .labelsHidden()
                                .onChange(of: barsZoneColors[i]) { _, newColor in
                                    guard let ctx = model.ctx else { return }
                                    let hex = colorToHex(newColor)
                                    hex.withCString { sparkamp_set_zone_color(ctx, Int32(i), $0) }
                                    sparkamp_save_config(ctx)
                                }
                        }
                    }
                }
            } else {
                Section("Waveform") {
                    Picker("Style", selection: $waveformStyle) {
                        Text("Lines").tag(0)
                        Text("Filled").tag(1)
                    }
                    .pickerStyle(.segmented)
                    .onChange(of: waveformStyle) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_waveform_style(ctx, Int32(newValue))
                        sparkamp_save_config(ctx)
                    }

                    Stepper("Zones: \(waveformZones)", value: $waveformZones, in: 1...6)
                        .onChange(of: waveformZones) { _, newValue in
                            guard let ctx = model.ctx else { return }
                            sparkamp_set_waveform_zones(ctx, Int32(newValue))
                            sparkamp_save_config(ctx)
                        }

                    ForEach(0..<waveformZones, id: \.self) { i in
                        HStack {
                            Text("Zone \(i + 1) color")
                            Spacer()
                            ColorPicker("", selection: $waveformZoneColors[i])
                                .labelsHidden()
                                .onChange(of: waveformZoneColors[i]) { _, newColor in
                                    guard let ctx = model.ctx else { return }
                                    let hex = colorToHex(newColor)
                                    hex.withCString { sparkamp_set_waveform_zone_color(ctx, Int32(i), $0) }
                                    sparkamp_save_config(ctx)
                                }
                        }
                    }
                }
            }
        }
        .formStyle(.grouped)
        .onAppear { loadFromFFI() }
    }

    private func loadFromFFI() {
        guard let ctx = model.ctx else { return }

        vizMode      = Int(sparkamp_get_viz_mode(ctx))
        barsMirror   = sparkamp_get_viz_mirror(ctx)
        barsZones    = Int(sparkamp_get_viz_zones(ctx)).clamped(to: 1...6)
        waveformStyle = Int(sparkamp_get_waveform_style(ctx))
        waveformZones = Int(sparkamp_get_waveform_zones(ctx)).clamped(to: 1...6)

        for i in 0..<6 {
            let ptr = sparkamp_get_zone_color(ctx, Int32(i))
            let hex = ptr.map { String(cString: $0) } ?? "#00ff00"
            sparkamp_free_string(ptr)
            barsZoneColors[i] = Color(hex: hex) ?? .green
        }

        for i in 0..<6 {
            let ptr = sparkamp_get_waveform_zone_color(ctx, Int32(i))
            let hex = ptr.map { String(cString: $0) } ?? "#00ff00"
            sparkamp_free_string(ptr)
            waveformZoneColors[i] = Color(hex: hex) ?? .green
        }
    }

    private func colorToHex(_ color: Color) -> String {
        let ns = NSColor(color).usingColorSpace(.sRGB) ?? NSColor(color)
        var r: CGFloat = 0, g: CGFloat = 0, b: CGFloat = 0, a: CGFloat = 0
        ns.getRed(&r, green: &g, blue: &b, alpha: &a)
        return String(format: "#%02x%02x%02x", Int(r * 255), Int(g * 255), Int(b * 255))
    }
}
