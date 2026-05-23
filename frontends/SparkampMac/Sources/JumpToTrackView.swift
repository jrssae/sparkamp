import SwiftUI
import AppKit

// MARK: - Jump-to-track window

/// Standalone search window: type to filter, ↑↓ to navigate, Enter to jump and play.
/// Uses the exact same PlaylistRow component and List styling as the playlist window.
/// Opened via `j` key or Playback menu; dismisses with Esc or after playing a track.
struct JumpToTrackView: View {
    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager

    @State private var query = ""
    @State private var selectedPlaylistIndex: Int? = nil
    @FocusState private var fieldFocused: Bool

    private var theme: SkinTheme { themeManager.currentTheme }

    /// Playlist items that match the current query (or all items when query is empty).
    private var filteredItems: [PlaylistItem] {
        if query.isEmpty { return model.playlistItems }
        let q = query
        return model.playlistItems.filter {
            $0.title.localizedCaseInsensitiveContains(q) ||
            $0.artist.localizedCaseInsensitiveContains(q) ||
            $0.albumArtist.localizedCaseInsensitiveContains(q)
        }
    }

    var body: some View {
        let vars = themeManager.currentVars
        return VStack(spacing: 0) {

            // ── Header — matches playlist header exactly ───────────────────────
            HStack {
                let n = filteredItems.count
                let total = model.playlistItems.count
                Text(query.isEmpty
                     ? "\(total) track\(total == 1 ? "" : "s")"
                     : "\(n) of \(total) tracks")
                    .font(vars.bodyFont)
                    .foregroundStyle(theme.playlistDurationText)
                Spacer()
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 5)
            .background(theme.playlistBg.opacity(0.7))

            Divider()
                .background(theme.windowBorder)

            // ── Search field ──────────────────────────────────────────────────
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass")
                    .font(.system(size: 11))
                    .foregroundStyle(theme.playlistDurationText)

                TextField("Search tracks…", text: $query)
                    .textFieldStyle(.plain)
                    .font(vars.bodyFont)
                    .foregroundStyle(theme.playlistText)
                    .focused($fieldFocused)
                    .onChange(of: query) { _, _ in
                        // Keep the selection valid when filter changes
                        if let sel = selectedPlaylistIndex,
                           !filteredItems.contains(where: { $0.id == sel }) {
                            selectedPlaylistIndex = filteredItems.first?.id
                        }
                    }
                    .onSubmit { playSelected() }

                if !query.isEmpty {
                    Button {
                        query = ""
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                            .foregroundStyle(theme.playlistDurationText)
                    }
                    .buttonStyle(.plain)
                }
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 7)
            .background(theme.lcdBackground)

            Divider()
                .background(theme.windowBorder)

            // ── Results list — identical styling to PlaylistView ──────────────
            List(selection: $selectedPlaylistIndex) {
                ForEach(filteredItems) { item in
                    PlaylistRow(item: item, isCurrent: item.id == model.currentIndex)
                        .listRowBackground(
                            item.id == model.currentIndex
                            ? theme.playlistCurrentBg
                            : Color.clear
                        )
                        .listRowInsets(EdgeInsets(top: 2, leading: 8, bottom: 2, trailing: 8))
                        .tag(item.id)
                }
            }
            .listStyle(.plain)
            .background(theme.playlistBg)
            .scrollContentBackground(.hidden)
            .onKeyPress(.return) { playSelected(); return .handled }
            // Arrow keys move the highlighted result up / down.  Routed
            // through hidden buttons with explicit keyboard shortcuts
            // because the TextField keeps focus during typing and would
            // otherwise consume arrow keys as text-cursor movement.  The
            // keyboardShortcut path catches them at the responder chain
            // level regardless of which inner view has focus.
            .background(arrowShortcutButtons)
            // Double-click on a row plays it immediately.  SwiftUI's
            // `contextMenu(forSelectionType:menu:primaryAction:)` is the
            // canonical way to attach a double-click handler to a List
            // with selection — the `menu` returns nothing so no actual
            // context menu appears on right-click.
            .contextMenu(forSelectionType: Int.self, menu: { _ in
                EmptyView()
            }, primaryAction: { ids in
                if let idx = ids.first {
                    selectedPlaylistIndex = idx
                    playSelected()
                }
            })

            Divider()
                .background(theme.windowBorder)
        }
        .background(theme.playlistBg)
        .preferredColorScheme(themeManager.preferredColorScheme)
        .onAppear {
            // Pre-select the currently playing track immediately.
            selectedPlaylistIndex = model.currentIndex >= 0 ? model.currentIndex : filteredItems.first?.id
            // Defer focus by one run-loop cycle: @FocusState only works after
            // the WindowGroup window becomes the key window, which hasn't
            // happened yet at onAppear time.
            DispatchQueue.main.async { fieldFocused = true }
        }
        .onDisappear {
            model.jumpToTrackVisible = false
        }
        .onKeyPress(.escape) {
            model.jumpToTrackVisible = false
            return .handled
        }
    }

    // MARK: Actions

    private func playSelected() {
        let idx = selectedPlaylistIndex ?? filteredItems.first?.id
        guard let idx else { return }
        model.jumpTo(index: idx)
        model.play()
        model.jumpToTrackVisible = false
    }

    /// Zero-size hidden buttons that bind ↑/↓ to selection movement.
    /// Same trick used in PlaylistView / ML editor for Delete: hidden
    /// keyboardShortcut buttons catch the keys at the responder chain
    /// regardless of which inner control has focus.
    private var arrowShortcutButtons: some View {
        ZStack {
            Button("", action: { moveSelection(by:  1) })
                .keyboardShortcut(.downArrow, modifiers: [])
            Button("", action: { moveSelection(by: -1) })
                .keyboardShortcut(.upArrow,   modifiers: [])
        }
        .frame(width: 0, height: 0)
        .opacity(0)
        .accessibilityHidden(true)
    }

    /// Shift `selectedPlaylistIndex` by `delta` positions through
    /// `filteredItems`, clamping at both ends.  Seeds with the first item
    /// when no row is selected yet (so the very first arrow press picks
    /// something visible).
    private func moveSelection(by delta: Int) {
        guard !filteredItems.isEmpty else { return }
        let currentRow = filteredItems.firstIndex(where: { $0.id == selectedPlaylistIndex })
        let next: Int
        if let cur = currentRow {
            next = min(max(cur + delta, 0), filteredItems.count - 1)
        } else {
            next = delta > 0 ? 0 : filteredItems.count - 1
        }
        selectedPlaylistIndex = filteredItems[next].id
    }
}
