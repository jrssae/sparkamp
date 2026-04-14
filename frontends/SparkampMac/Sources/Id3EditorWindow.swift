import SwiftUI
import AppKit

// MARK: - ID3 tag editor window

struct Id3EditorView: View {
    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager

    @State private var tagCtx: OpaquePointer? = nil
    @State private var filePath: String = ""
    @State private var isReadOnly: Bool = false
    @State private var saveStatus: String = ""

    // Standard tag fields
    @State private var title: String = ""
    @State private var artist: String = ""
    @State private var album: String = ""
    @State private var albumArtist: String = ""
    @State private var genre: String = ""
    @State private var year: String = ""
    @State private var trackNumber: String = ""
    @State private var discNumber: String = ""
    @State private var bpm: String = ""
    @State private var comment: String = ""

    @State private var extraFrames: [(id: String, value: String)] = []
    @State private var artwork: NSImage? = nil

    private var theme: SkinTheme { themeManager.currentTheme }

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
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(theme.background)

            Divider().background(theme.windowBorder)

            // ── Main content ──────────────────────────────────────────────────
            HStack(alignment: .top, spacing: 12) {
                // Artwork thumbnail
                if let img = artwork {
                    Image(nsImage: img)
                        .resizable()
                        .scaledToFit()
                        .frame(maxWidth: 100, maxHeight: 100)
                        .cornerRadius(4)
                        .overlay(
                            RoundedRectangle(cornerRadius: 4)
                                .stroke(theme.windowBorder, lineWidth: 1)
                        )
                        .padding(.top, 8)
                        .padding(.leading, 12)
                }

                // Tag form
                Form {
                    TextField("Title", text: $title)
                        .disabled(isReadOnly)
                    TextField("Artist", text: $artist)
                        .disabled(isReadOnly)
                    TextField("Album", text: $album)
                        .disabled(isReadOnly)
                    TextField("Album Artist", text: $albumArtist)
                        .disabled(isReadOnly)
                    TextField("Genre", text: $genre)
                        .disabled(isReadOnly)
                    TextField("Year", text: $year)
                        .disabled(isReadOnly)
                    TextField("Track Number", text: $trackNumber)
                        .disabled(isReadOnly)
                    TextField("Disc Number", text: $discNumber)
                        .disabled(isReadOnly)
                    TextField("BPM", text: $bpm)
                        .disabled(isReadOnly)
                    TextField("Comment", text: $comment)
                        .disabled(isReadOnly)

                    if !extraFrames.isEmpty {
                        DisclosureGroup("Custom Frames (\(extraFrames.count))") {
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
                            }
                        }
                    }
                }
                .formStyle(.grouped)
                .scrollContentBackground(.hidden)
            }

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
                    Button("Save") {
                        saveTag()
                    }
                    .buttonStyle(Id3ControlButtonStyle(theme: theme))
                    .disabled(tagCtx == nil)
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(theme.background)
        }
        .frame(minWidth: 420, idealWidth: 520, minHeight: 400)
        .background(theme.background)
        .preferredColorScheme(themeManager.preferredColorScheme)
        .onAppear {
            loadTag()
        }
        .onDisappear {
            if let t = tagCtx {
                sparkamp_tag_close(t)
                tagCtx = nil
            }
            model.id3EditorVisible = false
        }
        .onChange(of: model.id3TrackIndex) { _, _ in
            loadTag()
        }
    }

    // MARK: Load tag

    private func loadTag() {
        guard let ctx = model.ctx else { return }

        // Determine which track index to use
        let idx = model.id3TrackIndex >= 0 ? model.id3TrackIndex : model.currentIndex
        guard idx >= 0 else { return }

        // Get file path from FFI
        let pathPtr = sparkamp_playlist_get_path(ctx, Int32(idx))
        let path = pathPtr.map { String(cString: $0) } ?? ""
        sparkamp_free_string(pathPtr)
        guard !path.isEmpty else { return }
        filePath = path

        // Close any existing tag context
        if let existing = tagCtx {
            sparkamp_tag_close(existing)
            tagCtx = nil
        }

        // Open new tag context
        guard let newTag = path.withCString({ sparkamp_tag_open($0) }) else { return }
        tagCtx = newTag

        // Check writability
        isReadOnly = !FileManager.default.isWritableFile(atPath: path)

        // Read standard fields
        title       = readField(tag: newTag, frameId: "TIT2")
        artist      = readField(tag: newTag, frameId: "TPE1")
        album       = readField(tag: newTag, frameId: "TALB")
        albumArtist = readField(tag: newTag, frameId: "TPE2")
        genre       = readField(tag: newTag, frameId: "TCON")
        year        = readField(tag: newTag, frameId: "TDRC")
        trackNumber = readField(tag: newTag, frameId: "TRCK")
        discNumber  = readField(tag: newTag, frameId: "TPOS")
        bpm         = readField(tag: newTag, frameId: "TBPM")
        comment     = readField(tag: newTag, frameId: "COMM")

        // Read extra frames
        let standardIds: Set<String> = ["TIT2","TPE1","TALB","TPE2","TCON","TDRC","TRCK","TPOS","TBPM","COMM"]
        let frameCount = Int(sparkamp_tag_frame_count(newTag))
        extraFrames = (0..<frameCount).compactMap { i in
            let idPtr  = sparkamp_tag_frame_id(newTag, Int32(i))
            let valPtr = sparkamp_tag_frame_value(newTag, Int32(i))
            let frameId  = idPtr.map  { String(cString: $0) } ?? ""
            let frameVal = valPtr.map { String(cString: $0) } ?? ""
            sparkamp_free_string(idPtr)
            sparkamp_free_string(valPtr)
            guard !frameId.isEmpty, !standardIds.contains(frameId) else { return nil }
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
    }

    // MARK: Save tag

    private func saveTag() {
        guard let tag = tagCtx else { return }

        writeField(tag: tag, frameId: "TIT2", value: title)
        writeField(tag: tag, frameId: "TPE1", value: artist)
        writeField(tag: tag, frameId: "TALB", value: album)
        writeField(tag: tag, frameId: "TPE2", value: albumArtist)
        writeField(tag: tag, frameId: "TCON", value: genre)
        writeField(tag: tag, frameId: "TDRC", value: year)
        writeField(tag: tag, frameId: "TRCK", value: trackNumber)
        writeField(tag: tag, frameId: "TPOS", value: discNumber)
        writeField(tag: tag, frameId: "TBPM", value: bpm)
        writeField(tag: tag, frameId: "COMM", value: comment)

        let result = sparkamp_tag_save(tag)
        switch result {
        case 0:
            saveStatus = "Saved ✓"
            // Refresh playlist metadata for this track
            if let ctx = model.ctx {
                let idx = model.id3TrackIndex >= 0 ? model.id3TrackIndex : model.currentIndex
                if idx >= 0 {
                    sparkamp_scan_metadata(ctx, Int32(idx))
                }
            }
        case -1:
            saveStatus = "Read-only"
        default:
            saveStatus = "Save failed"
        }

        // Clear status after 3 seconds
        DispatchQueue.main.asyncAfter(deadline: .now() + 3) {
            saveStatus = ""
        }
    }

    // MARK: FFI helpers

    private func readField(tag: OpaquePointer, frameId: String) -> String {
        let ptr = frameId.withCString { sparkamp_tag_get(tag, $0) }
        let value = ptr.map { String(cString: $0) } ?? ""
        sparkamp_free_string(ptr)
        return value
    }

    private func writeField(tag: OpaquePointer, frameId: String, value: String) {
        frameId.withCString { fId in
            value.withCString { val in
                sparkamp_tag_set(tag, fId, val)
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
