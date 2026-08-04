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
                    Text("Version \(Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "")")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                    Text("A compact, fast, open-source Winamp-style music player with the backend built in Rust and support for UI in GNOME desktop with GTK4 & macOS with Swift.")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
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
                Button("GNU Affero General Public License v3 (AGPL-3.0)") {
                    NSWorkspace.shared.open(
                        URL(string: "https://www.gnu.org/licenses/agpl-3.0.html")!
                    )
                }
                .buttonStyle(.link)
                .font(.subheadline)
            }

            VStack(alignment: .leading, spacing: 6) {
                Text("Get the latest")
                    .font(.headline)
                Text("Source code, releases, and issue tracking are hosted on GitHub. Clone the repository or grab the latest build there.")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                Button("github.com/jrssae/sparkamp") {
                    NSWorkspace.shared.open(
                        URL(string: "https://github.com/jrssae/sparkamp")!
                    )
                }
                .buttonStyle(.link)
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

    @State private var entries: [ThemeManager.SkinEntry] = []
    @State private var selection: String? = nil
    @State private var errorMessage: String? = nil

    var body: some View {
        Form {
            Section("Skin") {
                List(entries, selection: $selection) { entry in
                    HStack {
                        Text(entry.displayName)
                        if entry.isBuiltin {
                            Text("(built-in)")
                                .foregroundStyle(.secondary)
                        }
                        Spacer()
                        if entry.name == themeManager.activeSkin {
                            Image(systemName: "checkmark.circle.fill")
                                .foregroundStyle(.tint)
                        }
                    }
                    .tag(entry.name)
                }
                .frame(minHeight: 180)
                .onChange(of: selection) { _, new in
                    if let new, new != themeManager.activeSkin {
                        themeManager.setActiveSkin(new)
                    }
                }

                HStack {
                    Button("Add skin…")     { addSkin() }
                    Button("Remove")        { removeSelected() }
                        .disabled(isBuiltinSelected)
                    Button("Download skin…") { downloadSelected() }
                        .disabled(selection == nil)
                }
            }

            Section("Documentation") {
                Button("Export how-to guide…") { exportGuide() }
                Text("A markdown reference describing every variable in the skin format and which UI elements it controls.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
        .alert("Could not add skin",
               isPresented: Binding(
                   get: { errorMessage != nil },
                   set: { if !$0 { errorMessage = nil } })) {
            Button("OK") { errorMessage = nil }
        } message: {
            Text(errorMessage ?? "")
        }
        .onAppear {
            entries = themeManager.listSkins()
            selection = themeManager.activeSkin
        }
    }

    // MARK: Actions

    private var isBuiltinSelected: Bool {
        guard let s = selection else { return true }
        return s == "dark" || s == "light"
    }

    private func addSkin() {
        let panel = NSOpenPanel()
        panel.title = "Add Sparkamp skin"
        panel.allowedContentTypes = [.init(filenameExtension: "css")!]
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.begin { response in
            guard response == .OK, let url = panel.url else { return }
            Task { @MainActor in
                switch themeManager.addUserSkin(from: url) {
                case .success(let entry):
                    entries = themeManager.listSkins()
                    themeManager.setActiveSkin(entry.name)
                    selection = entry.name
                case .failure(let err):
                    switch err {
                    case .unreadable:
                        errorMessage = "The selected file could not be read."
                    case .noRootBlock:
                        errorMessage = "The file is not a valid Sparkamp skin — missing a :root { } block."
                    case .copyFailed:
                        errorMessage = "Could not copy the skin into the user skins directory."
                    }
                }
            }
        }
    }

    private func removeSelected() {
        guard let s = selection, !isBuiltinSelected else { return }
        themeManager.hideSkin(s)
        entries = themeManager.listSkins()
        selection = themeManager.activeSkin
    }

    private func downloadSelected() {
        guard let s = selection else { return }
        let panel = NSSavePanel()
        panel.title = "Save Sparkamp skin"
        panel.nameFieldStringValue = "\(s).css"
        panel.allowedContentTypes = [.init(filenameExtension: "css")!]
        panel.begin { response in
            guard response == .OK, let url = panel.url else { return }
            Task { @MainActor in
                themeManager.exportSkin(s, to: url)
            }
        }
    }

    private func exportGuide() {
        let panel = NSSavePanel()
        panel.title = "Save Sparkamp skin guide"
        panel.nameFieldStringValue = "sparkamp-skin-guide.md"
        panel.allowedContentTypes = [.init(filenameExtension: "md")!]
        panel.begin { response in
            guard response == .OK, let url = panel.url else { return }
            Task { @MainActor in
                themeManager.exportGuide(to: url)
            }
        }
    }
}

// MARK: - Behavior pane

/// GTK's Behavior tab: how playback, adding files, ReplayGain, play counts,
/// fadeout and discs behave. The disc and gnudb settings live here rather than
/// under Media Library because that is where GTK keeps them.
private struct BehaviorPane: View {
    @EnvironmentObject var model: SparkampModel

    @State private var autoplayOnAdd: Bool = false
    @State private var addBehavior: Int    = 0    // 0=Append, 1=Replace
    @State private var playlistFormat: Int = 0    // 0=m3u8, 1=m3u

    // Discs / gnudb — GTK groups these under Behavior too.
    @State private var gnudbEmail: String = ""
    @State private var gnudbSubmitTest: Bool = true
    @State private var burnVerify: Bool = true
    @State private var autoShowInsertedCd: Bool = true

    // ReplayGain (playback normalization).
    @State private var rgEnabled: Bool     = true
    @State private var rgSource: Int       = 2    // 0=Track, 1=Album, 2=Automatic
    @State private var rgClip: Bool        = true
    @State private var rgFallback: Double  = 0.0

    // Play-count threshold (Phase 10, F11).
    @State private var playStatsEnabled: Bool = true
    @State private var playStatsMode: Int     = 0    // 0=Seconds, 1=Percent
    @State private var playStatsSeconds: Int  = 20
    @State private var playStatsPercent: Int  = 50
    @State private var fadeoutSeconds: Int    = 3

    var body: some View {
        Form {
            Section("Playlists") {
                Picker("Playlist format", selection: $playlistFormat) {
                    Text("m3u8 (UTF-8)").tag(0)
                    Text("m3u").tag(1)
                }
                .onChange(of: playlistFormat) { _, newValue in
                    guard let ctx = model.ctx else { return }
                    sparkamp_set_playlist_format(ctx, Int32(newValue))
                    sparkamp_save_config(ctx)
                }
                Text("New playlists, Save As, and device exports use this format. Existing playlists keep their own.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            Section("Playback") {
                Toggle("Autoplay on add", isOn: $autoplayOnAdd)
                    .onChange(of: autoplayOnAdd) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_autoplay_on_add(ctx, newValue)
                        sparkamp_save_config(ctx)
                    }

                Picker("Media library → playlist", selection: $addBehavior) {
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

            Section("ReplayGain") {
                Toggle("Use ReplayGain volume normalization", isOn: $rgEnabled)
                    .onChange(of: rgEnabled) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_rg_enabled(ctx, newValue)
                        sparkamp_save_config(ctx)
                    }

                Picker("ReplayGain source", selection: $rgSource) {
                    Text("Track").tag(0)
                    Text("Album").tag(1)
                    Text("Automatic").tag(2)
                }
                .onChange(of: rgSource) { _, newValue in
                    guard let ctx = model.ctx else { return }
                    sparkamp_set_rg_source(ctx, Int32(newValue))
                    sparkamp_save_config(ctx)
                }
                .disabled(!rgEnabled)

                Toggle("Clipping protection", isOn: $rgClip)
                    .onChange(of: rgClip) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_rg_clip_protection(ctx, newValue)
                        sparkamp_save_config(ctx)
                    }
                    .disabled(!rgEnabled)

                Stepper(
                    "Fallback gain (no RG info): \(rgFallback, specifier: "%.1f") dB",
                    value: $rgFallback, in: -15...15, step: 0.5
                )
                .onChange(of: rgFallback) { _, newValue in
                    guard let ctx = model.ctx else { return }
                    sparkamp_set_rg_fallback_db(ctx, Float(newValue))
                    sparkamp_save_config(ctx)
                }
                .disabled(!rgEnabled)

                Text("Applied to tracks that have no ReplayGain value. Automatic uses album gain unless shuffle is on.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            Section("Play Count") {
                Toggle("Count plays", isOn: $playStatsEnabled)
                    .onChange(of: playStatsEnabled) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_play_stats_enabled(ctx, newValue)
                        sparkamp_save_config(ctx)
                    }

                Picker("Count after", selection: $playStatsMode) {
                    Text("N seconds").tag(0)
                    Text("N% of track").tag(1)
                }
                .onChange(of: playStatsMode) { _, newValue in
                    guard let ctx = model.ctx else { return }
                    sparkamp_set_play_stats_mode(ctx, UInt32(newValue))
                    sparkamp_save_config(ctx)
                }
                .disabled(!playStatsEnabled)

                Stepper("After \(playStatsSeconds) seconds", value: $playStatsSeconds, in: 1...3600)
                    .onChange(of: playStatsSeconds) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_play_stats_seconds(ctx, UInt32(newValue))
                        sparkamp_save_config(ctx)
                    }
                    .disabled(!playStatsEnabled || playStatsMode != 0)

                Stepper("After \(playStatsPercent)% of track", value: $playStatsPercent, in: 1...100)
                    .onChange(of: playStatsPercent) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_play_stats_percent(ctx, UInt32(newValue))
                        sparkamp_save_config(ctx)
                    }
                    .disabled(!playStatsEnabled || playStatsMode != 1)

                Text("A track's play count and last-played date update in the Media Library once playback passes this point. Tracks shorter than the threshold count near the end.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            Section("Stop With Fadeout") {
                Stepper("Fade length (seconds): \(fadeoutSeconds)",
                        value: $fadeoutSeconds, in: 1...10)
                    .onChange(of: fadeoutSeconds) { _, newValue in
                        model.setFadeoutSeconds(newValue)
                    }

                Text("Shift+V ramps playback down to silence over this long and then stops. Plain Stop (v) is still immediate.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            // ── Discs ─────────────────────────────────────────────────────
            // Grouped here, not under Media Library, to match GTK's Behavior
            // tab.
            Section("gnudb submissions") {
                TextField("you@example.com", text: $gnudbEmail)
                    .textFieldStyle(.roundedBorder)
                    .autocorrectionDisabled()
                    .onSubmit { saveGnudbEmail() }
                Text("Your address, sent with gnudb disc submissions (gnudb requires a personal one — Sparkamp asks on the first submission if this is blank). Lookups work without it.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)

                Toggle("Submit in test mode", isOn: $gnudbSubmitTest)
                    .onChange(of: gnudbSubmitTest) { _, v in
                        if let ctx = model.ctx { sparkamp_set_gnudb_submit_test(ctx, v) }
                    }
                Text("gnudb validates test submissions without publishing them. Turn off once a real submission is confirmed working.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            Section("Disc burning") {
                Toggle("Verify discs after burning", isOn: $burnVerify)
                    .onChange(of: burnVerify) { _, v in
                        if let ctx = model.ctx { sparkamp_set_burn_verify(ctx, v) }
                    }
                Text("Reads the disc back after a burn to catch bad media. Slower; turn off to trade safety for speed.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            Section("Audio CD inserted") {
                Toggle("Open the Media Library", isOn: $autoShowInsertedCd)
                    .onChange(of: autoShowInsertedCd) { _, v in
                        if let ctx = model.ctx { sparkamp_set_auto_show_inserted_cd(ctx, v) }
                    }
                Text("Shows the Media Library at the drive that received the disc. To have macOS launch Sparkamp automatically on insert, set it as the handler in System Settings.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                Button("Open CDs & DVDs Settings…") { openCdDvdSettings() }
                    .buttonStyle(.bordered)
            }
        }
        .formStyle(.grouped)
        .onAppear {
            guard let ctx = model.ctx else { return }
            autoplayOnAdd  = sparkamp_get_autoplay_on_add(ctx)
            let p = sparkamp_get_gnudb_email(ctx)
            gnudbEmail = p.map { String(cString: $0) } ?? ""
            sparkamp_free_string(p)
            gnudbSubmitTest    = sparkamp_get_gnudb_submit_test(ctx)
            burnVerify         = sparkamp_get_burn_verify(ctx)
            autoShowInsertedCd = sparkamp_get_auto_show_inserted_cd(ctx)
            addBehavior    = Int(sparkamp_get_playlist_add_behavior(ctx))
            playlistFormat = Int(sparkamp_get_playlist_format(ctx))
            rgEnabled      = sparkamp_get_rg_enabled(ctx)
            rgSource       = Int(sparkamp_get_rg_source(ctx))
            rgClip         = sparkamp_get_rg_clip_protection(ctx)
            rgFallback     = Double(sparkamp_get_rg_fallback_db(ctx))
            playStatsEnabled = sparkamp_get_play_stats_enabled(ctx)
            playStatsMode    = Int(sparkamp_get_play_stats_mode(ctx))
            playStatsSeconds = Int(sparkamp_get_play_stats_seconds(ctx))
            playStatsPercent = Int(sparkamp_get_play_stats_percent(ctx))
            fadeoutSeconds   = Int(sparkamp_get_fadeout_secs(ctx))
        }
        // The email field commits on Return; catch the case where the pane
        // closes with an uncommitted edit still in it.
        .onDisappear { saveGnudbEmail() }
    }

    private func saveGnudbEmail() {
        guard let ctx = model.ctx else { return }
        gnudbEmail.withCString { sparkamp_set_gnudb_email(ctx, $0) }
    }

    /// Open the macOS "CDs & DVDs" pane, where the "When you insert a music CD"
    /// action lives. We never write that preference programmatically (it's in
    /// Apple's protected `com.apple.digihub` domain) — the user points it at
    /// Sparkamp.app once here.
    private func openCdDvdSettings() {
        let pane = URL(fileURLWithPath: "/System/Library/PreferencePanes/DigiHubDiscs.prefPane")
        if FileManager.default.fileExists(atPath: pane.path) {
            NSWorkspace.shared.open(pane)
        } else if let url = URL(string: "x-apple.systempreferences:com.apple.preferences.DigiHubDiscs") {
            NSWorkspace.shared.open(url)
        }
    }
}

// MARK: - Visualizer pane

private struct VisualizerPane: View {
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

// MARK: - Media Library pane

private struct MediaLibraryPane: View {
    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager

    /// Analyze ReplayGain for newly added/scanned files automatically.
    @State private var rgAutoAnalyze: Bool = false
    /// Write computed ReplayGain values back into MP3 tags (non-MP3 skipped).
    @State private var rgWriteTags: Bool = false

    // Watch folders (Phase 8 Task 9 FFI, wired here in Task 12). Defaults
    // mirror MediaLibraryConfig's Default impl (src/config.rs).
    @State private var watchFolders: Bool = true
    @State private var autoAddPlayed: Bool = false
    @State private var removeMissingOnRescan: Bool = false
    @State private var compactOnRescan: Bool = false
    @State private var rescanOnStartup: Bool = false
    /// Per-folder recurse flags, keyed by folder path, mirrored from the DB
    /// on appear and whenever the folder list changes.
    ///
    /// A binding that read `sparkamp_ml_folder_recurse` straight through on
    /// every render looked simpler, but it gave the checkbox no state
    /// SwiftUI could observe: it redrew correctly only because the model's
    /// 10 Hz tick happens to invalidate this pane, and it cost two SQLite
    /// queries per folder per redraw on the main thread. GTK reads the flag
    /// once as it builds each row too (`frontends/gtk/window/settings.rs`).
    @State private var folderRecurse: [String: Bool] = [:]
    /// F12.1: restore each Media-Library view's search query on next open.
    @State private var rememberSearch: Bool = false
    /// F12.2: display/group a track with no album-artist tag under its
    /// artist instead of blank.
    @State private var artistAsAlbumArtist: Bool = false
    /// F12.3: skip opening the Media-Library database at startup; it opens
    /// lazily on first demand (ML window, device sync, or the folder
    /// watcher's first need — see `openMediaLibrary()`). Inert on mac today:
    /// unlike GTK, mac has never eagerly opened the DB at startup (Phase 8
    /// baseline — `sparkamp_ml_open` is only ever called from demand sites),
    /// so this toggle mainly keeps the persisted config in sync across
    /// platforms rather than changing mac's own runtime behaviour.
    @State private var skipDbLoad: Bool = false

    var body: some View {
        let vars = themeManager.currentVars
        return Form {
            // ── Rescan ─────────────────────────────────────────────────────
            // Rescan and Cancel Scan swap places, the way GTK's pair does —
            // a running scan had no way to be stopped from here before.
            Section("Scan") {
                HStack {
                    if model.mlScanRunning {
                        Button("Cancel Scan") { model.mlCancelScan() }
                            .buttonStyle(.bordered)
                    } else {
                        Button("Rescan") {
                            model.openMediaLibrary()
                            model.mlRescanAll()
                        }
                        .buttonStyle(.borderedProminent)
                    }
                }
                if model.mlScanRunning {
                    ProgressView(
                        value: model.mlScanTotal > 0
                            ? Double(model.mlScanDone) / Double(model.mlScanTotal) : 0
                    )
                    Text(model.mlScanTotal > 0
                         ? "Scanning \(model.mlScanDone)/\(model.mlScanTotal)…"
                         : "Scanning…")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }

            // ── ReplayGain analysis ────────────────────────────────────────
            Section("ReplayGain") {
                HStack {
                    if model.rgRunning {
                        Button("Cancel Analysis") { model.rgCancelAnalyze() }
                            .buttonStyle(.bordered)
                    } else {
                        Button("Analyze ReplayGain") {
                            model.openMediaLibrary()
                            model.rgAnalyzeMissing()
                        }
                        .buttonStyle(.borderedProminent)

                        Button("Force Recalculate") {
                            model.openMediaLibrary()
                            let ids = model.mlAllTracks().map(\.id)
                            model.rgAnalyzeSelection(ids: ids)
                        }
                        .buttonStyle(.bordered)
                    }
                }
                if model.rgRunning {
                    ProgressView(
                        value: model.rgTotal > 0
                            ? Double(model.rgDone) / Double(model.rgTotal) : 0
                    )
                    Text(model.rgTotal > 0
                         ? "Analyzing \(model.rgDone)/\(model.rgTotal)…"
                         : "Analyzing ReplayGain…")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Toggle("Analyze ReplayGain on add/scan", isOn: $rgAutoAnalyze)
                    .onChange(of: rgAutoAnalyze) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_rg_auto_analyze(ctx, newValue)
                        sparkamp_save_config(ctx)
                    }
                Toggle("Write ReplayGain tags to files (MP3)", isOn: $rgWriteTags)
                    .onChange(of: rgWriteTags) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_rg_write_tags(ctx, newValue)
                        sparkamp_save_config(ctx)
                    }
            }

            // ── Folder watching (Phase 8 Task 12) ──────────────────────────
            Section("Folder Watching") {
                Toggle("Watch folders for changes", isOn: $watchFolders)
                    .onChange(of: watchFolders) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        // The Rust setter already (re)builds the watcher —
                        // no extra call needed here.
                        sparkamp_set_watch_folders(ctx, newValue)
                        sparkamp_save_config(ctx)
                    }
                Toggle("Automatically add played tracks", isOn: $autoAddPlayed)
                    .onChange(of: autoAddPlayed) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_auto_add_played(ctx, newValue)
                        sparkamp_save_config(ctx)
                    }
                Toggle("Remove missing files on rescan", isOn: $removeMissingOnRescan)
                    .onChange(of: removeMissingOnRescan) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_remove_missing_on_rescan(ctx, newValue)
                        sparkamp_save_config(ctx)
                    }
                Toggle("Compact database after rescan", isOn: $compactOnRescan)
                    .onChange(of: compactOnRescan) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_compact_on_rescan(ctx, newValue)
                        sparkamp_save_config(ctx)
                    }
                Toggle("Rescan all folders on startup", isOn: $rescanOnStartup)
                    .onChange(of: rescanOnStartup) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_rescan_on_startup(ctx, newValue)
                        sparkamp_save_config(ctx)
                    }
            }

            // ── Library behavior (F12.1–F12.3) ─────────────────────────────
            // Their own section: these three have nothing to do with folder
            // watching, and reading as if they did is misleading.
            Section("Library Behavior") {
                Toggle("Remember search per view", isOn: $rememberSearch)
                    .onChange(of: rememberSearch) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_remember_search(ctx, newValue)
                        sparkamp_save_config(ctx)
                    }
                Toggle("Treat artist as album artist", isOn: $artistAsAlbumArtist)
                    .onChange(of: artistAsAlbumArtist) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_artist_as_album_artist(ctx, newValue)
                        sparkamp_save_config(ctx)
                    }
                Toggle("Skip database load at startup", isOn: $skipDbLoad)
                    .onChange(of: skipDbLoad) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_skip_db_load(ctx, newValue)
                        sparkamp_save_config(ctx)
                    }
            }

            // ── Watched folders ────────────────────────────────────────────
            Section {
                if model.mlFolders.isEmpty {
                    Text("No folders added yet.")
                        .foregroundStyle(.secondary)
                        .font(vars.bodyFont)
                } else {
                    ForEach(model.mlFolders, id: \.self) { folder in
                        HStack {
                            Image(systemName: "folder")
                                .foregroundStyle(.secondary)
                            Text(folder)
                                .font(vars.bodyFont)
                                .lineLimit(1)
                                .truncationMode(.middle)
                            Spacer()
                            // Per-folder recurse (Phase 8 Task 12). Reads the
                            // mirror; writes through to the DB and rebuilds
                            // the watcher so the new mode takes effect on the
                            // live watch, not just the next scan.
                            Toggle("Recurse", isOn: Binding(
                                get: { folderRecurse[folder] ?? true },
                                set: { newValue in
                                    folderRecurse[folder] = newValue
                                    guard let ctx = model.ctx else { return }
                                    folder.withCString {
                                        sparkamp_ml_set_folder_recurse(ctx, $0, newValue)
                                    }
                                    sparkamp_ml_watch_rebuild(ctx)
                                }
                            ))
                            .toggleStyle(.checkbox)
                            .font(vars.bodyFont)
                            .help("Include subfolders when scanning/watching this folder")
                            Button {
                                model.mlRemoveFolder(folder)
                            } label: {
                                Image(systemName: "minus.circle")
                                    .foregroundStyle(.red)
                            }
                            .buttonStyle(.plain)
                        }
                    }
                }
            } header: {
                HStack {
                    Text("Watched Folders")
                    Spacer()
                    Button {
                        model.openMediaLibrary()
                        model.mlOpenAddFolderPicker()
                    } label: {
                        Label("Add Folder…", systemImage: "plus")
                            .font(vars.bodyFont)
                    }
                    .buttonStyle(.borderless)
                }
            }

            // Disc and gnudb settings live in the Behavior tab, where GTK
            // keeps them.

            // ── Tools ──────────────────────────────────────────────────────
            Section("Tools") {
                HStack {
                    VStack(alignment: .leading, spacing: 4) {
                        Text("Find Duplicates")
                            .font(vars.bodyFont.weight(.medium))
                        Text("Scan your media library for duplicate tracks using title, artist, and duration matching.")
                            .font(vars.bodyFont)
                            .foregroundStyle(.secondary)
                    }
                    Spacer()
                    Button("Scan…") {
                        model.dedupVisible = true
                    }
                    .buttonStyle(.bordered)
                }
                .padding(.vertical, 2)
            }
        }
        .formStyle(.grouped)
        .onAppear {
            // Ensure folder list is fresh when the pane is shown.
            if model.mlIsOpen { model.mlRefreshFolders() }
            if let ctx = model.ctx {
                rgAutoAnalyze = sparkamp_get_rg_auto_analyze(ctx)
                rgWriteTags = sparkamp_get_rg_write_tags(ctx)
                watchFolders = sparkamp_get_watch_folders(ctx)
                autoAddPlayed = sparkamp_get_auto_add_played(ctx)
                removeMissingOnRescan = sparkamp_get_remove_missing_on_rescan(ctx)
                compactOnRescan = sparkamp_get_compact_on_rescan(ctx)
                rescanOnStartup = sparkamp_get_rescan_on_startup(ctx)
                rememberSearch = sparkamp_get_remember_search(ctx)
                artistAsAlbumArtist = sparkamp_get_artist_as_album_artist(ctx)
                skipDbLoad = sparkamp_get_skip_db_load(ctx)
            }
            loadFolderRecurse()
        }
        .onChange(of: model.mlFolders) { _, _ in loadFolderRecurse() }
    }

    /// Re-read every watched folder's recurse flag into `folderRecurse`.
    /// Called on appear and whenever the folder list changes — the only two
    /// moments the flags can go stale, since nothing outside this pane
    /// writes them.
    private func loadFolderRecurse() {
        guard let ctx = model.ctx else { return }
        var flags: [String: Bool] = [:]
        for folder in model.mlFolders {
            flags[folder] = folder.withCString { sparkamp_ml_folder_recurse(ctx, $0) }
        }
        folderRecurse = flags
    }
}
