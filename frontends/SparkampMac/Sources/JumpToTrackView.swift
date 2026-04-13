import SwiftUI

// MARK: - Jump-to-track overlay

/// Live search sheet: type to filter, ↑↓ to navigate, Enter to jump and play, Esc to close.
///
/// Available from the main window (j key) and the fullscreen visualizer (j key).
/// Filters SparkampModel.playlistItems client-side — no extra FFI needed.
struct JumpToTrackView: View {
    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager
    @FocusState private var fieldFocused: Bool

    @State private var query        = ""
    @State private var selectedRow: Int = 0   // index into `filtered`

    private var theme: SkinTheme { themeManager.currentTheme }

    // Filtered playlist items with their original playlist indices.
    private var filtered: [(playlistIndex: Int, item: PlaylistItem)] {
        if query.isEmpty {
            return model.playlistItems.enumerated().map { (i, item) in (i, item) }
        }
        return model.playlistItems.enumerated().compactMap { (i, item) in
            let match = item.title.localizedCaseInsensitiveContains(query)
                     || item.artist.localizedCaseInsensitiveContains(query)
                     || item.albumArtist.localizedCaseInsensitiveContains(query)
            return match ? (i, item) : nil
        }
    }

    var body: some View {
        VStack(spacing: 0) {
            // ── Search field ──────────────────────────────────────────────
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass")
                    .font(.system(size: 13))
                    .foregroundStyle(theme.playlistText.opacity(0.6))

                TextField("Search tracks…", text: $query)
                    .textFieldStyle(.plain)
                    .font(.system(size: 13))
                    .foregroundStyle(theme.playlistText)
                    .focused($fieldFocused)
                    .onChange(of: query) { _, _ in selectedRow = 0 }
                    .onSubmit { playSelected() }

                if !query.isEmpty {
                    Button {
                        query = ""
                        selectedRow = 0
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                            .font(.system(size: 13))
                            .foregroundStyle(theme.playlistText.opacity(0.5))
                    }
                    .buttonStyle(.plain)
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
            .background(theme.lcdBackground)

            Divider()
                .background(theme.lcdBorder)

            // ── Results list ──────────────────────────────────────────────
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ForEach(Array(filtered.enumerated()), id: \.element.playlistIndex) { rowIdx, entry in
                            JumpRow(
                                item: entry.item,
                                isSelected: rowIdx == selectedRow,
                                theme: theme
                            )
                            .id(rowIdx)
                            .onTapGesture {
                                selectedRow = rowIdx
                                playSelected()
                            }
                        }

                        if filtered.isEmpty {
                            Text("No results")
                                .font(.system(size: 12))
                                .foregroundStyle(theme.playlistText.opacity(0.5))
                                .frame(maxWidth: .infinity)
                                .padding(.vertical, 24)
                        }
                    }
                }
                .onChange(of: selectedRow) { _, row in
                    withAnimation(.easeOut(duration: 0.1)) {
                        proxy.scrollTo(row, anchor: .center)
                    }
                }
            }
        }
        .background(theme.background)
        // Keyboard navigation handled at this level so it works regardless
        // of which child view has focus.
        .onKeyPress(.upArrow) {
            if selectedRow > 0 { selectedRow -= 1 }
            return .handled
        }
        .onKeyPress(.downArrow) {
            if selectedRow < filtered.count - 1 { selectedRow += 1 }
            return .handled
        }
        .onKeyPress(.return) {
            playSelected()
            return .handled
        }
        .onKeyPress(.escape) {
            model.jumpToTrackVisible = false
            return .handled
        }
        .onAppear {
            fieldFocused = true
            // Pre-select the currently playing track if the query is empty.
            if query.isEmpty, model.currentIndex >= 0 {
                let idx = filtered.firstIndex { $0.playlistIndex == model.currentIndex } ?? 0
                selectedRow = idx
            }
        }
    }

    // MARK: Actions

    private func playSelected() {
        guard selectedRow < filtered.count else { return }
        let playlistIndex = filtered[selectedRow].playlistIndex
        model.jumpTo(index: playlistIndex)
        model.play()
        model.jumpToTrackVisible = false
    }
}

// MARK: - Single result row

private struct JumpRow: View {
    let item: PlaylistItem
    let isSelected: Bool
    let theme: SkinTheme

    var body: some View {
        HStack(spacing: 10) {
            // Track indicator
            Image(systemName: isSelected ? "play.fill" : "music.note")
                .font(.system(size: 10))
                .foregroundStyle(isSelected ? theme.playlistCurrentText : theme.playlistText.opacity(0.4))
                .frame(width: 14)

            VStack(alignment: .leading, spacing: 1) {
                Text(item.title.isEmpty ? "Unknown" : item.title)
                    .font(.system(size: 12, weight: isSelected ? .semibold : .regular))
                    .foregroundStyle(isSelected ? theme.playlistCurrentText : theme.playlistText)
                    .lineLimit(1)

                if !item.artist.isEmpty || !item.albumArtist.isEmpty {
                    let artist = item.artist.isEmpty ? item.albumArtist : item.artist
                    Text(artist)
                        .font(.system(size: 11))
                        .foregroundStyle((isSelected ? theme.playlistCurrentText : theme.playlistText).opacity(0.65))
                        .lineLimit(1)
                }
            }

            Spacer()

            Text(item.durationString)
                .font(.system(size: 11, design: .monospaced))
                .foregroundStyle((isSelected ? theme.playlistCurrentText : theme.playlistText).opacity(0.6))
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 5)
        .background(
            isSelected
                ? theme.playlistCurrentBg.opacity(0.6)
                : Color.clear
        )
        .contentShape(Rectangle())
    }
}
