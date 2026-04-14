import SwiftUI
import AppKit

// MARK: - Settings window

struct SettingsView: View {
    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager

    // Appearance
    @State private var themeChoice: Int = 0   // 0=System, 1=Dark, 2=Light

    // Playback
    @State private var autoplayOnAdd: Bool = false
    @State private var addBehavior: Int = 0   // 0=Append, 1=Replace

    // Visualizer — bars
    @State private var vizMode: Int = 0       // 0=Bars, 1=Waveform
    @State private var barsZones: Int = 3
    @State private var barsZoneColors: [Color] = Array(repeating: .green, count: 6)

    // Visualizer — waveform
    @State private var waveformStyle: Int = 0  // 0=Lines, 1=Filled
    @State private var waveformZones: Int = 3
    @State private var waveformZoneColors: [Color] = Array(repeating: .green, count: 6)

    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        Form {
            // ── Appearance ────────────────────────────────────────────────────
            Section {
                Picker("Theme", selection: $themeChoice) {
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
            } header: {
                Text("Appearance")
            }

            // ── Playback ──────────────────────────────────────────────────────
            Section {
                Toggle("Autoplay when files are added", isOn: $autoplayOnAdd)
                    .onChange(of: autoplayOnAdd) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_autoplay_on_add(ctx, newValue)
                        sparkamp_save_config(ctx)
                    }

                Picker("When adding from Media Library", selection: $addBehavior) {
                    Text("Append").tag(0)
                    Text("Replace").tag(1)
                }
                .pickerStyle(.segmented)
                .onChange(of: addBehavior) { _, newValue in
                    guard let ctx = model.ctx else { return }
                    sparkamp_set_playlist_add_behavior(ctx, Int32(newValue))
                    sparkamp_save_config(ctx)
                }
            } header: {
                Text("Playback")
            }

            // ── Visualizer ────────────────────────────────────────────────────
            Section {
                Picker("Mode", selection: $vizMode) {
                    Text("Bars").tag(0)
                    Text("Waveform").tag(1)
                }
                .pickerStyle(.segmented)
                .onChange(of: vizMode) { _, newValue in
                    guard let ctx = model.ctx else { return }
                    sparkamp_set_viz_mode(ctx, Int32(newValue))
                    sparkamp_save_config(ctx)
                }

                if vizMode == 0 {
                    Stepper("Bars Zones: \(barsZones)", value: $barsZones, in: 1...6)
                        .onChange(of: barsZones) { _, newValue in
                            guard let ctx = model.ctx else { return }
                            sparkamp_set_viz_zones(ctx, Int32(newValue))
                            sparkamp_save_config(ctx)
                        }

                    ForEach(0..<barsZones, id: \.self) { i in
                        HStack {
                            Text("Zone \(i + 1)")
                            Spacer()
                            ColorPicker("", selection: $barsZoneColors[i])
                                .labelsHidden()
                                .onChange(of: barsZoneColors[i]) { _, newColor in
                                    guard let ctx = model.ctx else { return }
                                    let hex = colorToHex(newColor)
                                    hex.withCString { cStr in
                                        sparkamp_set_zone_color(ctx, Int32(i), cStr)
                                    }
                                    sparkamp_save_config(ctx)
                                }
                        }
                    }
                } else {
                    Picker("Waveform Style", selection: $waveformStyle) {
                        Text("Lines").tag(0)
                        Text("Filled").tag(1)
                    }
                    .pickerStyle(.segmented)
                    .onChange(of: waveformStyle) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_waveform_style(ctx, Int32(newValue))
                        sparkamp_save_config(ctx)
                    }

                    Stepper("Waveform Zones: \(waveformZones)", value: $waveformZones, in: 1...6)
                        .onChange(of: waveformZones) { _, newValue in
                            guard let ctx = model.ctx else { return }
                            sparkamp_set_waveform_zones(ctx, Int32(newValue))
                            sparkamp_save_config(ctx)
                        }

                    ForEach(0..<waveformZones, id: \.self) { i in
                        HStack {
                            Text("Zone \(i + 1)")
                            Spacer()
                            ColorPicker("", selection: $waveformZoneColors[i])
                                .labelsHidden()
                                .onChange(of: waveformZoneColors[i]) { _, newColor in
                                    guard let ctx = model.ctx else { return }
                                    let hex = colorToHex(newColor)
                                    hex.withCString { cStr in
                                        sparkamp_set_waveform_zone_color(ctx, Int32(i), cStr)
                                    }
                                    sparkamp_save_config(ctx)
                                }
                        }
                    }
                }
            } header: {
                Text("Visualizer")
            }

            // ── About ─────────────────────────────────────────────────────────
            Section {
                Text("Sparkamp — open source Winamp-style player")
                    .foregroundStyle(.secondary)
                Text("v0.3.0")
                    .foregroundStyle(.secondary)
            } header: {
                Text("About")
            }
        }
        .formStyle(.grouped)
        .frame(minWidth: 400, idealWidth: 480)
        .preferredColorScheme(themeManager.preferredColorScheme)
        .onAppear {
            loadFromFFI()
            // Sync theme choice from themeManager state
            switch themeManager.themeSource {
            case .dark:   themeChoice = 1
            case .light:  themeChoice = 2
            default:      themeChoice = 0
            }
        }
    }

    // MARK: Load from FFI

    private func loadFromFFI() {
        guard let ctx = model.ctx else { return }

        autoplayOnAdd = sparkamp_get_autoplay_on_add(ctx)
        addBehavior   = Int(sparkamp_get_playlist_add_behavior(ctx))
        vizMode       = Int(sparkamp_get_viz_mode(ctx))
        barsZones     = Int(sparkamp_get_viz_zones(ctx)).clamped(to: 1...6)
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

    // MARK: Color helpers

    private func colorToHex(_ color: Color) -> String {
        let ns = NSColor(color).usingColorSpace(.sRGB) ?? NSColor(color)
        var r: CGFloat = 0, g: CGFloat = 0, b: CGFloat = 0, a: CGFloat = 0
        ns.getRed(&r, green: &g, blue: &b, alpha: &a)
        let ri = Int(r * 255), gi = Int(g * 255), bi = Int(b * 255)
        return String(format: "#%02x%02x%02x", ri, gi, bi)
    }
}
