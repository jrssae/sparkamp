import SwiftUI
import AppKit

// MARK: - ID3 field configuration

struct ID3FieldConfig: Identifiable, Codable, Equatable {
    var id: String        // ID3 frame ID (e.g. "TIT2")
    var label: String
    var column: Int       // 0 = left column, 1 = right column
    var order: Int        // sort position within the column
    var visible: Bool
}

extension ID3FieldConfig {
    static let defaults: [ID3FieldConfig] = [
        // Left column
        ID3FieldConfig(id: "TIT2", label: "Title",        column: 0, order: 0, visible: true),
        ID3FieldConfig(id: "TPE1", label: "Artist",       column: 0, order: 1, visible: true),
        ID3FieldConfig(id: "TALB", label: "Album",        column: 0, order: 2, visible: true),
        ID3FieldConfig(id: "TPE2", label: "Album Artist", column: 0, order: 3, visible: true),
        ID3FieldConfig(id: "TCON", label: "Genre",        column: 0, order: 4, visible: true),
        ID3FieldConfig(id: "TCOM", label: "Composer",     column: 0, order: 5, visible: false),
        ID3FieldConfig(id: "TEXT", label: "Lyricist",     column: 0, order: 6, visible: false),
        ID3FieldConfig(id: "TIT3", label: "Subtitle",     column: 0, order: 7, visible: false),
        // Right column
        ID3FieldConfig(id: "TDRC", label: "Year",         column: 1, order: 0, visible: true),
        ID3FieldConfig(id: "TRCK", label: "Track #",      column: 1, order: 1, visible: true),
        ID3FieldConfig(id: "TPOS", label: "Disc #",       column: 1, order: 2, visible: true),
        ID3FieldConfig(id: "TBPM", label: "BPM",          column: 1, order: 3, visible: true),
        ID3FieldConfig(id: "COMM", label: "Comment",      column: 1, order: 4, visible: true),
        ID3FieldConfig(id: "TCOP", label: "Copyright",    column: 1, order: 5, visible: false),
        ID3FieldConfig(id: "TENC", label: "Encoded by",   column: 1, order: 6, visible: false),
        ID3FieldConfig(id: "TPUB", label: "Publisher",    column: 1, order: 7, visible: false),
        ID3FieldConfig(id: "TKEY", label: "Key",          column: 1, order: 8, visible: false),
        ID3FieldConfig(id: "TMOO", label: "Mood",         column: 1, order: 9, visible: false),
        ID3FieldConfig(id: "TLAN", label: "Language",     column: 1, order: 10, visible: false),
        ID3FieldConfig(id: "TSRC", label: "ISRC",         column: 1, order: 11, visible: false),
    ]
}

// MARK: - ID3 tag editor window

struct Id3EditorView: View {
    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager

    @State private var tagCtx: OpaquePointer? = nil
    @State private var filePath: String = ""
    @State private var isReadOnly: Bool = false
    @State private var saveStatus: String = ""

    /// All editable field values, keyed by frame ID.
    @State private var fieldValues: [String: String] = [:]
    @State private var extraFrames: [(id: String, value: String)] = []
    @State private var artwork: NSImage? = nil

    @State private var showCustomize = false

    /// Field layout config — persisted as JSON in UserDefaults.
    @AppStorage("sparkamp.id3.fieldConfig") private var configJSON: String = ""

    private var fieldConfigs: [ID3FieldConfig] {
        get {
            guard !configJSON.isEmpty,
                  let data = configJSON.data(using: .utf8),
                  let decoded = try? JSONDecoder().decode([ID3FieldConfig].self, from: data)
            else { return ID3FieldConfig.defaults }
            return decoded
        }
    }

    private func saveConfigs(_ configs: [ID3FieldConfig]) {
        if let data = try? JSONEncoder().encode(configs),
           let str = String(data: data, encoding: .utf8) {
            configJSON = str
        }
    }

    private var theme: SkinTheme { themeManager.currentTheme }

    // Fields visible in each column, sorted by order
    private var leftFields:  [ID3FieldConfig] { fieldConfigs.filter { $0.visible && $0.column == 0 }.sorted { $0.order < $1.order } }
    private var rightFields: [ID3FieldConfig] { fieldConfigs.filter { $0.visible && $0.column == 1 }.sorted { $0.order < $1.order } }

    var body: some View {
        VStack(spacing: 0) {
            // ── Header ────────────────────────────────────────────────────────
            HStack(spacing: 8) {
                Text(filePath.isEmpty ? "No file" : URL(fileURLWithPath: filePath).lastPathComponent)
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(theme.titleText)
                    .lineLimit(1)
                    .truncationMode(.middle)

                if isReadOnly {
                    Text("Read-only")
                        .font(.system(size: 9, weight: .medium))
                        .foregroundStyle(theme.background)
                        .padding(.horizontal, 5)
                        .padding(.vertical, 2)
                        .background(
                            RoundedRectangle(cornerRadius: 3)
                                .fill(theme.playlistDurationText)
                        )
                }

                Spacer()

                Button("Customize…") { showCustomize = true }
                    .buttonStyle(.borderless)
                    .font(.system(size: 11))
                    .foregroundStyle(theme.titleText.opacity(0.75))
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(theme.background)

            Divider().background(theme.windowBorder)

            // ── Main content ──────────────────────────────────────────────────
            ScrollView {
                HStack(alignment: .top, spacing: 0) {
                    // Artwork
                    if let img = artwork {
                        Image(nsImage: img)
                            .resizable()
                            .scaledToFit()
                            .frame(maxWidth: 88, maxHeight: 88)
                            .cornerRadius(4)
                            .overlay(
                                RoundedRectangle(cornerRadius: 4)
                                    .stroke(theme.windowBorder, lineWidth: 1)
                            )
                            .padding(.top, 12)
                            .padding(.leading, 12)
                            .padding(.trailing, 8)
                            .help("Click to view full size")
                            .onTapGesture {
                                model.artworkImage = img
                                model.artworkWindowVisible = true
                            }
                    }

                    // Left column
                    VStack(alignment: .leading, spacing: 0) {
                        ForEach(leftFields, id: \.id) { field in
                            FieldRow(label: field.label,
                                     value: binding(for: field.id),
                                     readOnly: isReadOnly,
                                     theme: theme)
                        }
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.leading, artwork == nil ? 12 : 0)

                    // Right column
                    VStack(alignment: .leading, spacing: 0) {
                        ForEach(rightFields, id: \.id) { field in
                            FieldRow(label: field.label,
                                     value: binding(for: field.id),
                                     readOnly: isReadOnly,
                                     theme: theme)
                        }
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.trailing, 12)
                }
                .padding(.vertical, 8)

                // Extra / unknown frames
                if !extraFrames.isEmpty {
                    Divider()
                        .padding(.horizontal, 12)
                    DisclosureGroup("Additional Frames (\(extraFrames.count))") {
                        ForEach(extraFrames, id: \.id) { frame in
                            HStack(alignment: .top, spacing: 8) {
                                Text(frame.id)
                                    .font(.system(size: 10, design: .monospaced))
                                    .foregroundStyle(theme.playlistDurationText)
                                    .frame(width: 40, alignment: .leading)
                                Text(frame.value)
                                    .font(.system(size: 10))
                                    .foregroundStyle(theme.playlistText)
                                    .lineLimit(2)
                            }
                            .padding(.vertical, 1)
                            .padding(.horizontal, 12)
                        }
                    }
                    .padding(.horizontal, 12)
                    .padding(.bottom, 8)
                }
            }
            .background(theme.lcdBackground)

            Divider().background(theme.windowBorder)

            // ── Bottom bar ────────────────────────────────────────────────────
            HStack(spacing: 10) {
                Spacer()

                if !saveStatus.isEmpty {
                    Text(saveStatus)
                        .font(.system(size: 11))
                        .foregroundStyle(saveStatus.contains("✓") ? Color.green : Color.red)
                }

                if !isReadOnly {
                    Button("Save") { saveTag() }
                        .buttonStyle(Id3ControlButtonStyle(theme: theme))
                        .disabled(tagCtx == nil)
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(theme.background)
        }
        .frame(minWidth: 520, idealWidth: 620, minHeight: 380)
        .background(theme.background)
        .preferredColorScheme(themeManager.preferredColorScheme)
        .onAppear { loadTag() }
        .onDisappear {
            if let t = tagCtx { sparkamp_tag_close(t); tagCtx = nil }
            model.id3EditorVisible = false
        }
        .onChange(of: model.id3TrackIndex) { _, _ in loadTag() }
        .sheet(isPresented: $showCustomize) {
            CustomizeFieldsSheet(configs: fieldConfigs) { updated in
                saveConfigs(updated)
            }
        }
    }

    // MARK: - Helpers

    private func binding(for frameId: String) -> Binding<String> {
        Binding(
            get: { fieldValues[frameId] ?? "" },
            set: { fieldValues[frameId] = $0 }
        )
    }

    // MARK: Load tag

    private func loadTag() {
        guard let ctx = model.ctx else { return }

        let idx = model.id3TrackIndex >= 0 ? model.id3TrackIndex : model.currentIndex
        guard idx >= 0 else { return }

        let pathPtr = sparkamp_playlist_get_path(ctx, Int32(idx))
        let path = pathPtr.map { String(cString: $0) } ?? ""
        sparkamp_free_string(pathPtr)
        guard !path.isEmpty else { return }
        filePath = path

        if let existing = tagCtx { sparkamp_tag_close(existing); tagCtx = nil }
        guard let newTag = path.withCString({ sparkamp_tag_open($0) }) else { return }
        tagCtx = newTag

        isReadOnly = !FileManager.default.isWritableFile(atPath: path)

        // Read all configured frame values
        var values: [String: String] = [:]
        for cfg in fieldConfigs {
            values[cfg.id] = readField(tag: newTag, frameId: cfg.id)
        }
        fieldValues = values

        // Collect extra (non-configured) frames
        let configuredIds = Set(fieldConfigs.map(\.id))
        let frameCount = Int(sparkamp_tag_frame_count(newTag))
        extraFrames = (0..<frameCount).compactMap { i in
            let idPtr  = sparkamp_tag_frame_id(newTag, Int32(i))
            let valPtr = sparkamp_tag_frame_value(newTag, Int32(i))
            let frameId  = idPtr.map  { String(cString: $0) } ?? ""
            let frameVal = valPtr.map { String(cString: $0) } ?? ""
            sparkamp_free_string(idPtr)
            sparkamp_free_string(valPtr)
            guard !frameId.isEmpty, !configuredIds.contains(frameId) else { return nil }
            return (id: frameId, value: frameVal)
        }

        // Read artwork
        artwork = nil
        var artLen: Int32 = 0
        if let artPtr = sparkamp_tag_get_artwork_data(newTag, &artLen), artLen > 0 {
            let data = Data(bytes: artPtr, count: Int(artLen))
            artwork = NSImage(data: data)
            sparkamp_tag_free_artwork(artPtr, artLen)
        }
        model.artworkImage = artwork
    }

    // MARK: Save tag

    private func saveTag() {
        guard let tag = tagCtx else { return }

        for cfg in fieldConfigs {
            writeField(tag: tag, frameId: cfg.id, value: fieldValues[cfg.id] ?? "")
        }

        let result = sparkamp_tag_save(tag)
        switch result {
        case 0:
            saveStatus = "Saved ✓"
            if let ctx = model.ctx {
                let idx = model.id3TrackIndex >= 0 ? model.id3TrackIndex : model.currentIndex
                if idx >= 0 { sparkamp_scan_metadata(ctx, Int32(idx)) }
            }
        case -1: saveStatus = "Read-only"
        default:  saveStatus = "Save failed"
        }

        DispatchQueue.main.asyncAfter(deadline: .now() + 3) { saveStatus = "" }
    }

    // MARK: FFI helpers

    private func readField(tag: OpaquePointer, frameId: String) -> String {
        let ptr = frameId.withCString { sparkamp_tag_get(tag, $0) }
        let value = ptr.map { String(cString: $0) } ?? ""
        sparkamp_free_string(ptr)
        return value
    }

    private func writeField(tag: OpaquePointer, frameId: String, value: String) {
        frameId.withCString { fId in value.withCString { val in sparkamp_tag_set(tag, fId, val) } }
    }
}

// MARK: - Inline field row

private struct FieldRow: View {
    let label: String
    @Binding var value: String
    let readOnly: Bool
    let theme: SkinTheme

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label)
                .font(.system(size: 9, weight: .semibold))
                .foregroundStyle(theme.playlistDurationText)
                .padding(.leading, 2)
            TextField("", text: $value)
                .textFieldStyle(.roundedBorder)
                .font(.system(size: 11))
                .disabled(readOnly)
        }
        .padding(.horizontal, 6)
        .padding(.vertical, 4)
    }
}

// MARK: - Customize fields sheet

private struct CustomizeFieldsSheet: View {
    @State private var configs: [ID3FieldConfig]
    let onSave: ([ID3FieldConfig]) -> Void

    @Environment(\.dismiss) private var dismiss

    init(configs: [ID3FieldConfig], onSave: @escaping ([ID3FieldConfig]) -> Void) {
        _configs = State(initialValue: configs)
        self.onSave = onSave
    }

    private var leftConfigs:  [ID3FieldConfig] { configs.filter { $0.column == 0 }.sorted { $0.order < $1.order } }
    private var rightConfigs: [ID3FieldConfig] { configs.filter { $0.column == 1 }.sorted { $0.order < $1.order } }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text("Customize Fields")
                    .font(.headline)
                Spacer()
                Button("Done") {
                    onSave(configs)
                    dismiss()
                }
                .keyboardShortcut(.defaultAction)
            }
            .padding()

            Divider()

            HStack(alignment: .top, spacing: 0) {
                // Left column list
                ColumnCustomizeList(
                    title: "Left Column",
                    configs: leftConfigs,
                    allConfigs: $configs
                )

                Divider()

                // Right column list
                ColumnCustomizeList(
                    title: "Right Column",
                    configs: rightConfigs,
                    allConfigs: $configs
                )
            }
        }
        .frame(width: 540, height: 400)
    }
}

// MARK: - Per-column customize list

private struct ColumnCustomizeList: View {
    let title: String
    let configs: [ID3FieldConfig]   // sorted items for this column
    @Binding var allConfigs: [ID3FieldConfig]

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(title)
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(.secondary)
                .padding(.horizontal, 12)
                .padding(.vertical, 8)

            List {
                ForEach(configs, id: \.id) { cfg in
                    HStack(spacing: 8) {
                        // Visibility toggle
                        Toggle("", isOn: visibleBinding(for: cfg.id))
                            .toggleStyle(.checkbox)
                            .labelsHidden()

                        VStack(alignment: .leading, spacing: 1) {
                            Text(cfg.label)
                                .font(.system(size: 12))
                            Text(cfg.id)
                                .font(.system(size: 9, design: .monospaced))
                                .foregroundStyle(.secondary)
                        }

                        Spacer()

                        // Move to other column
                        Button(cfg.column == 0 ? "→" : "←") {
                            moveToOtherColumn(id: cfg.id)
                        }
                        .buttonStyle(.borderless)
                        .font(.system(size: 11))
                        .foregroundStyle(.secondary)
                        .help(cfg.column == 0 ? "Move to right column" : "Move to left column")
                    }
                    .padding(.vertical, 2)
                }
                .onMove { indices, dest in
                    reorder(in: configs, from: indices, to: dest)
                }
            }
            .listStyle(.inset)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private func visibleBinding(for id: String) -> Binding<Bool> {
        Binding(
            get: { allConfigs.first(where: { $0.id == id })?.visible ?? true },
            set: { newVal in
                if let i = allConfigs.firstIndex(where: { $0.id == id }) {
                    allConfigs[i].visible = newVal
                }
            }
        )
    }

    private func moveToOtherColumn(id: String) {
        guard let i = allConfigs.firstIndex(where: { $0.id == id }) else { return }
        let newCol = allConfigs[i].column == 0 ? 1 : 0
        // Append at end of destination column
        let maxOrder = allConfigs.filter { $0.column == newCol }.map(\.order).max() ?? -1
        allConfigs[i].column = newCol
        allConfigs[i].order  = maxOrder + 1
        renumber()
    }

    private func reorder(in columnConfigs: [ID3FieldConfig], from source: IndexSet, to dest: Int) {
        var ordered = columnConfigs
        ordered.move(fromOffsets: source, toOffset: dest)
        // Update order values for items in this column
        for (newOrder, cfg) in ordered.enumerated() {
            if let i = allConfigs.firstIndex(where: { $0.id == cfg.id }) {
                allConfigs[i].order = newOrder
            }
        }
    }

    private func renumber() {
        for col in 0...1 {
            let items = allConfigs.filter { $0.column == col }.sorted { $0.order < $1.order }
            for (order, cfg) in items.enumerated() {
                if let i = allConfigs.firstIndex(where: { $0.id == cfg.id }) {
                    allConfigs[i].order = order
                }
            }
        }
    }
}

// MARK: - ID3 control button style

private struct Id3ControlButtonStyle: ButtonStyle {
    let theme: SkinTheme

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 11))
            .foregroundStyle(theme.modeBtnText)
            .padding(.horizontal, 10)
            .padding(.vertical, 4)
            .background(
                RoundedRectangle(cornerRadius: 4)
                    .fill(configuration.isPressed ? theme.transportActiveBg : theme.transportBg)
                    .overlay(
                        RoundedRectangle(cornerRadius: 4)
                            .stroke(theme.windowBorder, lineWidth: 1)
                    )
            )
            .opacity(configuration.isPressed ? 0.8 : 1.0)
    }
}
