import SwiftUI
import UniformTypeIdentifiers

// MARK: - Main player window

struct PlayerWindow: View {
    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager
    @Environment(\.openWindow)    var openWindow
    @Environment(\.dismissWindow) var dismissWindow
    @Environment(\.colorScheme)   var colorScheme

    @State private var isDraggingSeek = false
    @State private var seekPreview: Double = 0
    @State private var isFileTargeted = false

    private var theme: SkinTheme { themeManager.currentTheme }

    var body: some View {
        VStack(spacing: 0) {
            // ┌──────────────────────────────────────┐
            // │ [TIME large] │ Marquee title          │
            // │              │   [spring] [RPT] [SHF] │
            // │              │ [🔊 vol≈1/3] [PL][logo]│
            // ├──────────────────────────────────────┤
            // │ [seek bar full width]                 │
            // ├──────────────────────────────────────┤
            // │ [◀ ▶ ⏸ ⏹ ▶]                        │
            // └──────────────────────────────────────┘
            infoPanel
            seekRow
            transportRow
        }
        .frame(width: 480)
        .background(theme.background)
        .overlay(
            RoundedRectangle(cornerRadius: 0)
                .stroke(theme.windowBorder, lineWidth: 1)
                .allowsHitTesting(false)
        )
        .onDrop(of: [.fileURL], isTargeted: $isFileTargeted) { providers in
            handleDrop(providers: providers)
        }
        .overlay(dropOverlay)
        .onReceive(NotificationCenter.default.publisher(for: .openFilePicker)) { _ in
            model.openFilePicker()
        }
        .onAppear { model.refreshAll() }
        .onChange(of: model.playlistVisible) { _, visible in
            if visible { openWindow(id: "playlist") }
            else       { dismissWindow(id: "playlist") }
        }
        .contextMenu { themeMenu }
    }

    // MARK: – Info Panel
    // Left column: large tappable time. Right column (VStack): marquee, mode row, vol row.
    // Mirrors the Linux Sparkamp / Winamp layout exactly.

    private var infoPanel: some View {
        ZStack {
            theme.lcdBackground

            HStack(spacing: 0) {

                // ── Left column: time display ────────────────────────────────
                Button { model.toggleRemainingTime() } label: {
                    VStack(alignment: .center, spacing: 2) {
                        Text(timeDisplay)
                            .font(.system(size: 28, weight: .bold, design: .monospaced))
                            .foregroundStyle(theme.timeText)
                            .lineLimit(1)
                            .minimumScaleFactor(0.5)
                            .fixedSize()
                        Text(model.showRemainingTime ? "REMAIN" : "ELAPSED")
                            .font(.system(size: 7, weight: .medium))
                            .foregroundStyle(theme.timeText.opacity(0.5))
                            .fixedSize()
                    }
                    .frame(width: 118)
                    .padding(.vertical, 8)
                }
                .buttonStyle(.plain)
                .help("Click to toggle remaining / elapsed time")

                // ── Divider ──────────────────────────────────────────────────
                Rectangle()
                    .fill(theme.lcdBorder)
                    .frame(width: 1)
                    .padding(.vertical, 6)

                // ── Right column: song info + controls ───────────────────────
                VStack(alignment: .leading, spacing: 5) {

                    // Row 1 — state icon + scrolling "Artist — Title"
                    HStack(spacing: 5) {
                        Image(systemName: stateIcon)
                            .font(.system(size: 9, weight: .bold))
                            .foregroundStyle(stateColor)
                            .frame(width: 12)
                        MarqueeView(text: marqueeText)
                    }

                    // Row 2 — repeat + shuffle, right-aligned
                    HStack(spacing: 5) {
                        Spacer()
                        ModeButton(label: repeatLabel, isActive: model.repeatMode != 0) {
                            model.cycleRepeat()
                        }
                        .help("Cycle repeat (r)")
                        ModeButton(icon: "shuffle", isActive: model.shuffleEnabled) {
                            model.toggleShuffle()
                        }
                        .help("Toggle shuffle (s)")
                    }

                    // Row 3 — volume slider (~1/3 player width) + playlist + logo
                    HStack(spacing: 6) {
                        Image(systemName: "speaker.fill")
                            .font(.system(size: 9))
                            .foregroundStyle(theme.volumeThumb.opacity(0.7))

                        ThemedVolumeSlider(
                            value: Binding(get: { model.volume },
                                           set: { model.setVolume($0) })
                        )
                        // ~1/3 of the 480px player minus left col (118) and padding
                        .frame(maxWidth: 140)

                        Image(systemName: "speaker.wave.2.fill")
                            .font(.system(size: 9))
                            .foregroundStyle(theme.volumeThumb.opacity(0.7))

                        Spacer()

                        ModeButton(icon: "list.bullet", isActive: model.playlistVisible) {
                            model.playlistVisible.toggle()
                        }
                        .help("Show / hide Playlist (p)")

                        // App icon logo (42 px, matching Linux Sparkamp)
                        Image(nsImage: NSApp.applicationIconImage)
                            .resizable()
                            .interpolation(.high)
                            .frame(width: 42, height: 42)
                            .cornerRadius(8)
                            .help("Sparkamp")
                    }
                }
                .padding(.horizontal, 10)
                .padding(.vertical, 8)
                .frame(maxWidth: .infinity)
            }
        }
        .overlay(
            RoundedRectangle(cornerRadius: 0)
                .stroke(theme.lcdBorder, lineWidth: 1)
                .allowsHitTesting(false)
        )
    }

    // MARK: – Seek row

    private var seekRow: some View {
        ThemedSeekBar(
            position: model.position,
            duration: model.duration,
            isDragging: $isDraggingSeek,
            seekPreview: $seekPreview,
            onSeek: { model.seek(to: $0) }
        )
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
    }

    // MARK: – Transport row (prev ▶ ⏸ ⏹ next — full width, left-aligned)

    private var transportRow: some View {
        HStack(spacing: 8) {
            SkinButton(id: "prev",  icon: "backward.end.fill",  iconSize: 14) { model.prev() }
            SkinButton(id: "play",  icon: "play.fill",          iconSize: 16,
                       isHighlighted: model.isPlaying)  { model.play() }
            SkinButton(id: "pause", icon: "pause.fill",         iconSize: 14,
                       isHighlighted: model.isPaused)   { model.pause() }
            SkinButton(id: "stop",  icon: "stop.fill",          iconSize: 14) { model.stop() }
            SkinButton(id: "next",  icon: "forward.end.fill",   iconSize: 14) { model.next() }
            Spacer()
        }
        .padding(.horizontal, 10)
        .padding(.top, 6)
        .padding(.bottom, 8)
    }

    // MARK: – Drop overlay

    @ViewBuilder
    private var dropOverlay: some View {
        if isFileTargeted {
            RoundedRectangle(cornerRadius: 0)
                .stroke(theme.seekThumb, lineWidth: 2)
                .background(theme.seekThumb.opacity(0.06))
        }
    }

    // MARK: – Context menu (right-click / two-finger tap)

    @ViewBuilder
    private var themeMenu: some View {
        Section("Theme") {
            Button {
                themeManager.useSystem(colorScheme: colorScheme)
            } label: {
                Label(themeManager.themeSource == .system ? "✓ System Default" : "System Default",
                      systemImage: "macwindow")
            }
            Button {
                themeManager.useDark()
            } label: {
                Label(themeManager.themeSource == .dark ? "✓ Dark" : "Dark",
                      systemImage: "moon.fill")
            }
            Button {
                themeManager.useLight()
            } label: {
                Label(themeManager.themeSource == .light ? "✓ Light" : "Light",
                      systemImage: "sun.max.fill")
            }
        }
        Divider()
        Button("Load Skin (CSS)…") {
            themeManager.openSkinPicker(colorScheme: colorScheme)
        }
        Button("Export Default Skin…") {
            themeManager.exportDefaultCSS()
        }
        if case .custom(_) = themeManager.themeSource {
            Button("Remove Custom Skin", role: .destructive) {
                themeManager.removeCustomSkin(colorScheme: colorScheme)
            }
        }
    }

    // MARK: – Helpers

    private var stateIcon: String {
        if model.isPlaying { return "play.fill" }
        if model.isPaused  { return "pause.fill" }
        return "stop.fill"
    }

    private var stateColor: Color {
        if model.isPlaying { return theme.titleText }
        if model.isPaused  { return Color(hex: "#ffaa00") ?? .orange }
        return theme.modeBtnText
    }

    /// Marquee shows "Artist — Title" (or just title when no artist).
    private var marqueeText: String {
        if model.currentTitle.isEmpty { return "Sparkamp" }
        if !model.currentArtist.isEmpty {
            return "\(model.currentArtist) — \(model.currentTitle)"
        }
        return model.currentTitle
    }

    /// Large time string: elapsed or (negative) remaining.
    private var timeDisplay: String {
        if model.showRemainingTime, model.duration > 0 {
            let remaining = max(0, model.duration - model.position)
            return "−" + formatDuration(remaining)
        }
        return formatDuration(model.position)
    }

    private var repeatLabel: String {
        switch model.repeatMode {
        case 1: return "RPT1"
        case 2: return "RPTA"
        default: return "RPT"
        }
    }

    private func handleDrop(providers: [NSItemProvider]) -> Bool {
        let group = DispatchGroup()
        var urls: [URL] = []
        for p in providers {
            group.enter()
            p.loadItem(forTypeIdentifier: UTType.fileURL.identifier) { item, _ in
                if let data = item as? Data,
                   let url = URL(dataRepresentation: data, relativeTo: nil) {
                    urls.append(url)
                }
                group.leave()
            }
        }
        group.notify(queue: .main) { model.addFiles(urls) }
        return true
    }
}

// MARK: - Mode button (repeat / shuffle / playlist)

struct ModeButton: View {
    var label: String? = nil
    var icon: String? = nil
    let isActive: Bool
    let action: () -> Void

    @EnvironmentObject var themeManager: ThemeManager
    @State private var isHovered = false

    private var theme: SkinTheme { themeManager.currentTheme }

    var body: some View {
        Button(action: action) {
            Group {
                if let icon {
                    Image(systemName: icon)
                        .font(.system(size: 10, weight: .medium))
                } else if let label {
                    Text(label)
                        .font(.system(size: 9, weight: .bold))
                }
            }
            .foregroundStyle(isActive ? theme.modeBtnActiveText : theme.modeBtnText)
            .frame(minWidth: 24, minHeight: 18)
            .padding(.horizontal, 4)
            .background(
                RoundedRectangle(cornerRadius: 3)
                    .fill(isActive
                          ? theme.modeBtnActiveBg
                          : isHovered ? theme.transportHoverBg : theme.modeBtnBg)
                    .overlay(
                        RoundedRectangle(cornerRadius: 3)
                            .stroke(isActive
                                    ? theme.modeBtnActiveText.opacity(0.4)
                                    : theme.modeBtnBorder,
                                    lineWidth: 1)
                    )
            )
        }
        .buttonStyle(.plain)
        .onHover { isHovered = $0 }
    }
}

// MARK: - Themed seek bar

struct ThemedSeekBar: View {
    let position: Double
    let duration: Double
    @Binding var isDragging: Bool
    @Binding var seekPreview: Double
    let onSeek: (Double) -> Void

    @EnvironmentObject var themeManager: ThemeManager
    @State private var isHovered = false

    private var fraction: Double {
        guard duration > 0 else { return 0 }
        return (position / duration).clamped(to: 0...1)
    }
    private var displayFraction: Double { isDragging ? seekPreview : fraction }

    var body: some View {
        let t = themeManager.currentTheme
        let trackH: CGFloat = 4
        let thumbD: CGFloat = isHovered || isDragging ? 13 : 9

        GeometryReader { geo in
            let W     = geo.size.width
            let midY  = geo.size.height / 2
            let pad   = thumbD / 2
            let fillW = CGFloat(displayFraction) * (W - thumbD)
            let thumbX = pad + fillW

            ZStack(alignment: .leading) {
                Capsule()
                    .fill(t.seekTrack)
                    .frame(height: trackH)
                    .padding(.horizontal, pad)

                Capsule()
                    .fill(t.seekFill)
                    .frame(width: max(pad, fillW + pad), height: trackH)

                Circle()
                    .fill(t.seekThumb)
                    .frame(width: thumbD, height: thumbD)
                    .shadow(color: t.seekThumb.opacity(0.4), radius: 2)
                    .position(x: thumbX, y: midY)
                    .animation(.easeOut(duration: 0.08), value: thumbD)
            }
            .contentShape(Rectangle())
            .onHover { isHovered = $0 }
            .gesture(
                DragGesture(minimumDistance: 0)
                    .onChanged { v in
                        isDragging = true
                        seekPreview = ((v.location.x - pad) / (W - thumbD))
                            .clamped(to: 0...1)
                    }
                    .onEnded { v in
                        isDragging = false
                        let f = ((v.location.x - pad) / (W - thumbD)).clamped(to: 0...1)
                        onSeek(f)
                    }
            )
        }
        .frame(height: 18)
    }
}

// MARK: - Themed volume slider

struct ThemedVolumeSlider: View {
    @Binding var value: Double
    @EnvironmentObject var themeManager: ThemeManager

    var body: some View {
        Slider(value: $value, in: 0...1)
            .tint(themeManager.currentTheme.volumeThumb)
    }
}
