import SwiftUI
import AppKit

// MARK: - Equalizer window

struct EqualizerView: View {
    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager

    @State private var bands: [Double] = Array(repeating: 0, count: 10)
    @State private var preamp: Double = 1.0
    @State private var eqEnabled: Bool = false
    @State private var selectedPreset: Int = -1
    @State private var presetNames: [String] = []

    private var theme: SkinTheme { themeManager.currentTheme }

    var body: some View {
        VStack(spacing: 0) {
            // ── Header ────────────────────────────────────────────────────────
            HStack {
                Text("Equalizer")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(theme.titleText)
                Spacer()
                Toggle("Enabled", isOn: $eqEnabled)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                    .labelsHidden()
                    .onChange(of: eqEnabled) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_eq_enabled(ctx, newValue)
                        sparkamp_save_config(ctx)
                    }
                Text("Enable")
                    .font(.system(size: 10))
                    .foregroundStyle(theme.transportText)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(theme.background)

            Divider().background(theme.windowBorder)

            // ── Unavailable message ───────────────────────────────────────────
            if model.ctx == nil || !sparkamp_has_eq(model.ctx!) {
                VStack(spacing: 8) {
                    Image(systemName: "exclamationmark.triangle")
                        .font(.system(size: 24))
                        .foregroundStyle(theme.playlistDurationText)
                    Text("EQ not available (GStreamer equalizer-10bands not found)")
                        .font(.system(size: 11))
                        .foregroundStyle(theme.playlistDurationText)
                        .multilineTextAlignment(.center)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .padding(24)
                .background(theme.background)
            } else {
                // ── Preset row ────────────────────────────────────────────────
                HStack(spacing: 8) {
                    Text("Preset:")
                        .font(.system(size: 10))
                        .foregroundStyle(theme.transportText)

                    Picker("Preset", selection: $selectedPreset) {
                        Text("Custom").tag(-1)
                        ForEach(presetNames.indices, id: \.self) { i in
                            Text(presetNames[i]).tag(i)
                        }
                    }
                    .pickerStyle(.menu)
                    .controlSize(.small)
                    .frame(maxWidth: 160)

                    Button("Apply") {
                        guard selectedPreset >= 0, let ctx = model.ctx else { return }
                        sparkamp_apply_eq_preset(ctx, Int32(selectedPreset))
                        loadBandsFromFFI()
                        sparkamp_save_config(ctx)
                    }
                    .buttonStyle(EQControlButtonStyle(theme: theme))
                    .disabled(selectedPreset < 0)

                    Spacer()

                    Button("Reset") {
                        guard let ctx = model.ctx else { return }
                        sparkamp_reset_eq(ctx)
                        loadBandsFromFFI()
                        sparkamp_save_config(ctx)
                    }
                    .buttonStyle(EQControlButtonStyle(theme: theme))
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
                .background(theme.background)

                Divider().background(theme.windowBorder)

                // ── Band sliders ──────────────────────────────────────────────
                HStack(alignment: .bottom, spacing: 4) {
                    ForEach(0..<10, id: \.self) { i in
                        BandSliderColumn(
                            bandIndex: i,
                            value: $bands[i],
                            theme: theme,
                            onChange: { newVal in
                                guard let ctx = model.ctx else { return }
                                sparkamp_set_eq_band(ctx, Int32(i), Float(newVal))
                                sparkamp_save_config(ctx)
                                selectedPreset = -1
                            }
                        )
                    }
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 10)
                .background(theme.lcdBackground)

                Divider().background(theme.windowBorder)

                // ── Pre-amp row ───────────────────────────────────────────────
                HStack(spacing: 8) {
                    Text("Pre-amp:")
                        .font(.system(size: 10))
                        .foregroundStyle(theme.transportText)
                        .frame(width: 52, alignment: .leading)

                    Slider(value: $preamp, in: 0.5...1.5, step: 0.01)
                        .onChange(of: preamp) { _, newVal in
                            guard let ctx = model.ctx else { return }
                            sparkamp_set_preamp(ctx, Float(newVal))
                            sparkamp_save_config(ctx)
                        }

                    Text("\(Int(preamp * 100))%")
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundStyle(theme.transportText)
                        .frame(width: 36, alignment: .trailing)
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
                .background(theme.background)
            }
        }
        .frame(width: 460)
        .background(theme.background)
        .overlay(
            RoundedRectangle(cornerRadius: 0)
                .stroke(theme.windowBorder, lineWidth: 1)
        )
        .preferredColorScheme(themeManager.preferredColorScheme)
        .onAppear {
            loadAllFromFFI()
        }
    }

    // MARK: Load helpers

    private func loadAllFromFFI() {
        guard let ctx = model.ctx, sparkamp_has_eq(ctx) else { return }
        eqEnabled = sparkamp_get_eq_enabled(ctx)
        preamp = Double(sparkamp_get_preamp(ctx))
        loadBandsFromFFI()
        loadPresetNamesFromFFI()
    }

    private func loadBandsFromFFI() {
        guard let ctx = model.ctx else { return }
        for i in 0..<10 {
            bands[i] = Double(sparkamp_get_eq_band(ctx, Int32(i)))
        }
    }

    private func loadPresetNamesFromFFI() {
        guard let ctx = model.ctx else { return }
        let count = Int(sparkamp_eq_preset_count(ctx))
        presetNames = (0..<count).compactMap { i in
            let ptr = sparkamp_eq_preset_name(ctx, Int32(i))
            let name = ptr.map { String(cString: $0) }
            sparkamp_free_string(ptr)
            return name
        }
    }
}

// MARK: - Band slider column

private struct BandSliderColumn: View {
    let bandIndex: Int
    @Binding var value: Double
    let theme: SkinTheme
    let onChange: (Double) -> Void

    @State private var labelText: String = ""

    var body: some View {
        VStack(spacing: 3) {
            // dB value label
            Text(dbLabel)
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(theme.transportText)
                .frame(width: 36)

            // Vertical slider via rotation
            Slider(value: $value, in: -12...12, step: 0.1)
                .rotationEffect(.degrees(-90))
                .frame(width: 36, height: 120)
                .onChange(of: value) { _, newVal in
                    onChange(newVal)
                }

            // Frequency label
            Text(labelText)
                .font(.system(size: 8))
                .foregroundStyle(theme.playlistDurationText)
                .frame(width: 36)
                .lineLimit(1)
                .minimumScaleFactor(0.6)
        }
        .onAppear {
            let ptr = sparkamp_eq_band_label(Int32(bandIndex))
            labelText = ptr.map { String(cString: $0) } ?? "\(bandIndex)"
            sparkamp_free_string(ptr)
        }
    }

    private var dbLabel: String {
        let v = value
        if v >= 0 { return String(format: "+%.1f", v) }
        return String(format: "%.1f", v)
    }
}

// MARK: - EQ control button style

private struct EQControlButtonStyle: ButtonStyle {
    let theme: SkinTheme

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 10))
            .foregroundStyle(theme.modeBtnText)
            .padding(.horizontal, 8)
            .padding(.vertical, 3)
            .background(
                RoundedRectangle(cornerRadius: 3)
                    .fill(configuration.isPressed ? theme.transportActiveBg : theme.transportBg)
                    .overlay(
                        RoundedRectangle(cornerRadius: 3)
                            .stroke(theme.windowBorder, lineWidth: 1)
                    )
            )
            .opacity(configuration.isPressed ? 0.8 : 1.0)
    }
}
