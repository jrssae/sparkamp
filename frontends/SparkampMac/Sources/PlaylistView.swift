import SwiftUI
import UniformTypeIdentifiers

// MARK: - Playlist view

struct PlaylistView: View {
    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager
    @State private var selection: Int? = nil

    private var theme: SkinTheme { themeManager.currentTheme }

    var body: some View {
        VStack(spacing: 0) {
            // Track count header
            HStack {
                Text("\(model.playlistItems.count) track\(model.playlistItems.count == 1 ? "" : "s")")
                    .font(.system(size: 10))
                    .foregroundStyle(theme.playlistDurationText)
                Spacer()
                if let total = totalDuration {
                    Text(total)
                        .font(.system(size: 10))
                        .foregroundStyle(theme.playlistDurationText)
                }
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 5)
            .background(theme.playlistBg.opacity(0.7))

            Divider()
                .background(theme.windowBorder)

            List(selection: $selection) {
                ForEach(model.playlistItems) { item in
                    PlaylistRow(item: item, isCurrent: item.id == model.currentIndex)
                        .listRowBackground(
                            item.id == model.currentIndex
                            ? theme.playlistCurrentBg
                            : Color.clear
                        )
                        .listRowInsets(EdgeInsets(top: 2, leading: 8, bottom: 2, trailing: 8))
                        .tag(item.id)
                }
                .onMove  { from, to in model.moveTrack(from: from, to: to) }
                .onDelete { indexSet in
                    for i in indexSet.sorted().reversed() { model.removeTrack(at: i) }
                }
            }
            .listStyle(.plain)
            .background(theme.playlistBg)
            .scrollContentBackground(.hidden)
            .onKeyPress(.deleteForward) { deleteSelected(); return .handled }
            .onKeyPress(.delete)        { deleteSelected(); return .handled }
            .onKeyPress(.return)        { playSelected();   return .handled }
            .onChange(of: selection)    { }
            .onDrop(of: [.fileURL], isTargeted: nil) { providers in
                handleDrop(providers: providers)
            }
            .contextMenu(forSelectionType: Int.self, menu: { items in
                if let idx = items.first {
                    Button("Play") { model.jumpTo(index: idx) }
                    Button("Edit Tags…") { model.openId3Editor(trackIndex: idx) }
                    Divider()
                    Button("Remove", role: .destructive) { model.removeTrack(at: idx) }
                }
            }, primaryAction: { items in
                if let idx = items.first { model.jumpTo(index: idx) }
            })

            Divider()
                .background(theme.windowBorder)

            // ── Bottom control bar ────────────────────────────────────────────
            bottomBar
        }
        .background(theme.playlistBg)
        .preferredColorScheme(themeManager.preferredColorScheme)
        .onDisappear {
            // Sync model flag when window is closed via the system X button
            // so the playlist button in the player reflects the correct state.
            model.playlistVisible = false
        }
    }

    // MARK: Bottom control bar

    private var bottomBar: some View {
        HStack(spacing: 6) {
            // Left side: Add Files, Add Folder
            Button {
                model.openFilePicker()
            } label: {
                Label("Add Files", systemImage: "plus")
                    .font(.system(size: 10))
            }
            .buttonStyle(PlaylistControlButtonStyle(theme: theme))
            .help("Add audio files to playlist")

            Button {
                model.openFolderPicker()
            } label: {
                Label("Add Folder", systemImage: "folder.badge.plus")
                    .font(.system(size: 10))
            }
            .buttonStyle(PlaylistControlButtonStyle(theme: theme))
            .help("Add all audio files in a folder")

            Spacer()

            // Right side: Remove Selected, Remove All
            Button {
                if let idx = selection { model.removeTrack(at: idx); selection = nil }
            } label: {
                Label("Remove", systemImage: "minus")
                    .font(.system(size: 10))
            }
            .buttonStyle(PlaylistControlButtonStyle(theme: theme))
            .disabled(selection == nil)
            .help("Remove selected track")

            Button {
                model.clearPlaylist()
                selection = nil
            } label: {
                Label("Remove All", systemImage: "trash")
                    .font(.system(size: 10))
            }
            .buttonStyle(PlaylistControlButtonStyle(theme: theme))
            .disabled(model.playlistItems.isEmpty)
            .help("Clear entire playlist")
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 6)
        .background(theme.playlistBg.opacity(0.85))
    }

    // MARK: Helpers

    private var totalDuration: String? {
        let total = model.playlistItems.reduce(0.0) { $0 + max($1.duration, 0) }
        guard total > 0 else { return nil }
        return formatDuration(total)
    }

    private func deleteSelected() {
        if let idx = selection { model.removeTrack(at: idx); selection = nil }
    }

    private func playSelected() {
        if let idx = selection { model.jumpTo(index: idx) }
    }

    private func handleDrop(providers: [NSItemProvider]) -> Bool {
        let group = DispatchGroup()
        var urls: [URL] = []
        for p in providers {
            group.enter()
            p.loadItem(forTypeIdentifier: UTType.fileURL.identifier) { item, _ in
                if let data = item as? Data, let url = URL(dataRepresentation: data, relativeTo: nil) {
                    urls.append(url)
                }
                group.leave()
            }
        }
        group.notify(queue: .main) { model.addFiles(urls) }
        return true
    }
}

// MARK: - Playlist row (single-line: "Artist — Title")

struct PlaylistRow: View {
    let item: PlaylistItem
    let isCurrent: Bool

    @EnvironmentObject var themeManager: ThemeManager
    private var theme: SkinTheme { themeManager.currentTheme }

    var body: some View {
        HStack(spacing: 6) {
            // State / broken / read-only indicator
            Group {
                if isCurrent {
                    Image(systemName: "waveform")
                        .font(.system(size: 9))
                        .foregroundStyle(theme.playlistCurrentText)
                } else if item.broken {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .font(.system(size: 9))
                        .foregroundStyle(theme.playlistBrokenText)
                } else if item.fileMissing {
                    Image(systemName: "xmark.circle.fill")
                        .font(.system(size: 9))
                        .foregroundStyle(.red)
                } else if item.readOnly {
                    Image(systemName: "lock.fill")
                        .font(.system(size: 9))
                        .foregroundStyle(theme.playlistDurationText)
                } else {
                    Color.clear
                }
            }
            .frame(width: 12)

            // Single-line display: "Artist — Title"
            Text(item.displayName)
                .font(.system(size: 12))
                .foregroundStyle(
                    isCurrent ? theme.playlistCurrentText
                    : item.broken ? theme.playlistBrokenText
                    : theme.playlistText
                )
                .lineLimit(1)
                .truncationMode(.tail)

            Spacer()

            // Duration
            Text(item.durationString)
                .font(.system(size: 10))
                .foregroundStyle(theme.playlistDurationText)
        }
        .contentShape(Rectangle())
    }
}

// MARK: - Playlist control button style

struct PlaylistControlButtonStyle: ButtonStyle {
    let theme: SkinTheme

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .foregroundStyle(theme.modeBtnText)
            .padding(.horizontal, 6)
            .padding(.vertical, 4)
            .background(
                RoundedRectangle(cornerRadius: 3)
                    .fill(configuration.isPressed ? theme.transportActiveBg : theme.modeBtnBg)
                    .overlay(
                        RoundedRectangle(cornerRadius: 3)
                            .stroke(theme.modeBtnBorder, lineWidth: 1)
                    )
            )
            .opacity(configuration.isPressed ? 0.8 : 1.0)
    }
}
