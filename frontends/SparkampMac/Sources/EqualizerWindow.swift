import SwiftUI
import AppKit

// Slider ranges sourced from the Rust core so the UI never lets the user
// pick a value the engine will silently clamp.  These are evaluated once at
// process start; the core's clamp constants are immutable per build.
private let preampMin: Double  = sparkamp_preamp_min()
private let preampMax: Double  = sparkamp_preamp_max()
private let eqBandLimit: Double = sparkamp_eq_band_db_limit()

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
        let vars = themeManager.currentVars
        return VStack(spacing: 0) {
            // ── Header ────────────────────────────────────────────────────────
            HStack {
                Text("Equalizer")
                    .font(vars.bodyFont.weight(.semibold))
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
                    .font(vars.bodyFont)
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
                        .font(vars.bodyFont)
                        .foregroundStyle(theme.playlistDurationText)
                        .multilineTextAlignment(.center)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .padding(24)
                .background(theme.background)
            } else {
                // ── Pre-amp row ───────────────────────────────────────────────
                HStack(spacing: 8) {
                    Text("Pre-amp:")
                        .font(vars.bodyFont)
                        .foregroundStyle(theme.transportText)
                        .frame(width: 52, alignment: .leading)

                    Slider(value: $preamp, in: preampMin...preampMax, step: 0.01)
                        .onChange(of: preamp) { _, newVal in
                            guard let ctx = model.ctx else { return }
                            sparkamp_set_preamp(ctx, Float(newVal))
                            sparkamp_save_config(ctx)
                        }

                    Text("\(Int(preamp * 100))%")
                        .font(vars.smallMonospaceFont)
                        .foregroundStyle(theme.transportText)
                        .frame(width: 36, alignment: .trailing)
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
                .background(theme.background)

                Divider().background(theme.windowBorder)

                // ── Preset row ────────────────────────────────────────────────
                HStack(spacing: 8) {
                    Text("Preset:")
                        .font(vars.bodyFont)
                        .foregroundStyle(theme.transportText)

                    Picker("", selection: $selectedPreset) {
                        Text("Custom").tag(-1)
                        ForEach(presetNames.indices, id: \.self) { i in
                            Text(presetNames[i]).tag(i)
                        }
                    }
                    .pickerStyle(.menu)
                    .labelsHidden()
                    .controlSize(.small)
                    .frame(maxWidth: 160)
                    .onChange(of: selectedPreset) { _, newValue in
                        guard newValue >= 0, let ctx = model.ctx else { return }
                        sparkamp_apply_eq_preset(ctx, Int32(newValue))
                        loadBandsFromFFI()
                        sparkamp_save_config(ctx)
                    }

                    Spacer()

                    Button("Reset") {
                        guard let ctx = model.ctx else { return }
                        sparkamp_reset_eq(ctx)
                        loadBandsFromFFI()
                        selectedPreset = -1
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
                    Spacer(minLength: 0)
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 10)
                .background(theme.lcdBackground)
                .clipped()
            }
        }
        .frame(width: 460)
        .background(theme.background)
        .preferredColorScheme(themeManager.preferredColorScheme)
        .onAppear {
            loadAllFromFFI()
        }
        .onDisappear {
            // Sync model flag when window is closed via the system X button.
            model.equalizerVisible = false
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
            // Vertical slider via rotation.
            // The first .frame sets the track length (160 pt); rotationEffect
            // turns it 90°; the second .frame gives it a bounding box that
            // fits the column width while preserving the 160-pt travel.
            Slider(value: $value, in: -eqBandLimit...eqBandLimit, step: 0.1)
                .frame(width: 160)
                .rotationEffect(.degrees(-90))
                .frame(width: 36, height: 160)
                .clipped()
                .onChange(of: value) { _, newVal in
                    onChange(newVal)
                }

            // Frequency label
            Text(labelText)
                .font(theme.vars.bodyFont)
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
}

// MARK: - EQ control button style

private struct EQControlButtonStyle: ButtonStyle {
    let theme: SkinTheme

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(theme.vars.bodyFont)
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
