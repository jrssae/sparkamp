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
                case .about:        AboutPane()
                case .appearance:   AppearancePane()
                case .playback:     PlaybackPane()
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

private enum SettingsTab: String, CaseIterable {
    case about, appearance, playback, visualizer, mediaLibrary

    var label: String {
        switch self {
        case .about:         return "About"
        case .appearance:    return "Appearance"
        case .playback:      return "Playback"
        case .visualizer:    return "Visualizer"
        case .mediaLibrary:  return "Media Library"
        }
    }

    var icon: String {
        switch self {
        case .about:         return "info.circle"
        case .appearance:    return "paintbrush"
        case .playback:      return "play.circle"
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
                    Text("Version 0.3.0")
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

// MARK: - Media Library pane

private struct MediaLibraryPane: View {
    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager

    var body: some View {
        let vars = themeManager.currentVars
        return Form {
            // ── Open / rescan ──────────────────────────────────────────────
            Section("Library") {
                HStack {
                    Button("Open Media Library") {
                        model.openMediaLibrary()
                        model.mediaLibraryVisible = true
                    }
                    .buttonStyle(.borderedProminent)

                    Button("Rescan All") {
                        model.openMediaLibrary()
                        model.mlRescanAll()
                    }
                    .buttonStyle(.bordered)
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
        }
    }
}
