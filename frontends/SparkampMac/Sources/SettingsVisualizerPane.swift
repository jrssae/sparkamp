import SwiftUI
import AppKit

// Split out of SettingsWindow.swift on 2026-08-12: the file was 1,087
// lines of five unrelated panes. They were already siblings of
// `SettingsView` rather than nested in it, so each moved whole — the
// only edit is `private struct` -> `struct`, because file-private would
// now hide the pane from the host that instantiates it.

// MARK: - Visualizer pane

struct VisualizerPane: View {
    @EnvironmentObject var model: SparkampModel

    @State private var vizMode: Int          = 0     // 0=Bars, 1=Waveform, 2=Granite
    @State private var keepScreenAwake: Bool = true
    @State private var barsMirror: Bool      = true
    @State private var barsZones: Int        = 3
    @State private var barsZoneColors: [Color]     = Array(repeating: .green, count: 6)
    @State private var waveformStyle: Int    = 0     // 0=Lines, 1=Filled
    @State private var waveformZones: Int    = 3
    @State private var waveformZoneColors: [Color] = Array(repeating: .green, count: 6)
    @State private var granitePalette: Int   = 0     // 0=Granite…7=Spectrum
    @State private var graniteSpeed: Double  = 1.0
    @State private var graniteFeedback: Double = 0.6
    @State private var graniteEffect: Int    = 0     // 0=Plasma…11=Flag
    @State private var graniteAutoSwitch: Bool = true
    @State private var graniteBeatSens: Double = 1.5
    @State private var graniteBeatBright: Bool = true

    private static let granitePaletteNames =
        ["Granite", "Fire", "Neon", "Ocean", "Violet", "Sunset", "CRT", "Spectrum"]
    // Effect names live in GraniteCatalog (shared with the fullscreen FPS
    // overlay) so the two lists can't drift.

    var body: some View {
        Form {
            Section("Mode") {
                Picker("Visualizer mode", selection: $vizMode) {
                    Text("Bars").tag(0)
                    Text("Waveform").tag(1)
                    Text("Granite").tag(2)
                }
                .pickerStyle(.segmented)
                .onChange(of: vizMode) { _, newValue in
                    guard let ctx = model.ctx else { return }
                    sparkamp_set_viz_mode(ctx, Int32(newValue))
                    sparkamp_save_config(ctx)
                }

                Toggle("Keep display awake in fullscreen",
                       isOn: $keepScreenAwake)
                    .onChange(of: keepScreenAwake) { _, newValue in
                        model.setKeepScreenAwake(newValue)
                    }
                Text("When off (or the display is slept manually), fullscreen exits to the player instead of fighting macOS over the wake-up Space.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            if vizMode == 2 {
                Section("Granite") {
                    // Credit where it's due: Granite is a re-creation, not
                    // an original idea.
                    Text("Granite is an interpretation of the Geiss Winamp plugin by Ryan Geiss. All credit to his amazing work on the original. [Click](https://www.geisswerks.com/geiss/) for more information.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)

                    Picker("Palette", selection: $granitePalette) {
                        ForEach(Array(Self.granitePaletteNames.enumerated()), id: \.offset) {
                            idx, name in
                            Text(name).tag(idx)
                        }
                    }
                    .onChange(of: granitePalette) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_granite_palette(ctx, Int32(newValue))
                        sparkamp_save_config(ctx)
                    }

                    HStack {
                        Text("Speed")
                        Slider(value: $graniteSpeed, in: 0.1...5.0, step: 0.1)
                            .onChange(of: graniteSpeed) { _, newValue in
                                guard let ctx = model.ctx else { return }
                                sparkamp_set_granite_speed(ctx, Float(newValue))
                                sparkamp_save_config(ctx)
                            }
                        Text(String(format: "%.1f×", graniteSpeed))
                            .frame(width: 48, alignment: .trailing)
                    }

                    HStack {
                        Text("Feedback")
                        Slider(value: $graniteFeedback, in: 0.0...0.9, step: 0.05)
                            .onChange(of: graniteFeedback) { _, newValue in
                                guard let ctx = model.ctx else { return }
                                sparkamp_set_granite_feedback(ctx, Float(newValue))
                                sparkamp_save_config(ctx)
                            }
                        Text(String(format: "%.2f", graniteFeedback))
                            .frame(width: 48, alignment: .trailing)
                    }

                    Picker("Effect", selection: $graniteEffect) {
                        ForEach(Array(GraniteCatalog.effectNames.enumerated()), id: \.offset) {
                            idx, name in
                            Text(name).tag(idx)
                        }
                    }
                    .onChange(of: graniteEffect) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_granite_effect(ctx, Int32(newValue))
                        sparkamp_save_config(ctx)
                    }

                    Toggle("Auto-switch effect",
                           isOn: $graniteAutoSwitch)
                        .onChange(of: graniteAutoSwitch) { _, newValue in
                            guard let ctx = model.ctx else { return }
                            sparkamp_set_granite_auto_switch(ctx, newValue)
                            sparkamp_save_config(ctx)
                        }

                    HStack {
                        Text("Beat sensitivity")
                        Slider(value: $graniteBeatSens, in: 1.05...3.0, step: 0.05)
                            .onChange(of: graniteBeatSens) { _, newValue in
                                guard let ctx = model.ctx else { return }
                                sparkamp_set_granite_beat_sensitivity(ctx, Float(newValue))
                                sparkamp_save_config(ctx)
                            }
                        Text(String(format: "%.2f", graniteBeatSens))
                            .frame(width: 48, alignment: .trailing)
                    }
                    Text("Lower = more beats detected. Watch BPM in the fullscreen overlay (g).")
                        .font(.caption)
                        .foregroundStyle(.secondary)

                    Toggle("Brighten colors on beats", isOn: $graniteBeatBright)
                        .onChange(of: graniteBeatBright) { _, newValue in
                            guard let ctx = model.ctx else { return }
                            sparkamp_set_granite_beat_brightness(ctx, newValue)
                            sparkamp_save_config(ctx)
                        }
                }
            } else if vizMode == 0 {
                Section("Bars") {
                    Toggle("Mirror bars", isOn: $barsMirror)
                        .onChange(of: barsMirror) { _, newValue in
                            guard let ctx = model.ctx else { return }
                            sparkamp_set_viz_mirror(ctx, newValue)
                            sparkamp_save_config(ctx)
                        }

                    Stepper("Color zones: \(barsZones)", value: $barsZones, in: 1...6)
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

                    Stepper("Color zones: \(waveformZones)", value: $waveformZones, in: 1...6)
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
        keepScreenAwake = sparkamp_get_keep_screen_awake(ctx)
        granitePalette = Int(sparkamp_get_granite_palette(ctx)).clamped(to: 0...7)
        graniteSpeed   = Double(sparkamp_get_granite_speed(ctx))
        graniteFeedback = Double(sparkamp_get_granite_feedback(ctx))
        graniteEffect = Int(sparkamp_get_granite_effect(ctx)).clamped(to: 0...11)
        graniteAutoSwitch = sparkamp_get_granite_auto_switch(ctx)
        graniteBeatSens = Double(sparkamp_get_granite_beat_sensitivity(ctx))
        graniteBeatBright = sparkamp_get_granite_beat_brightness(ctx)

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
