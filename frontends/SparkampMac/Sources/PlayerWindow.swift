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
            lcdPanel
            seekRow
            transportRow
            modeRow
            bottomRow
        }
        .frame(width: 480)
        .background(theme.background)
        .overlay(
            RoundedRectangle(cornerRadius: 0)
                .stroke(theme.windowBorder, lineWidth: 1)
                .allowsHitTesting(false)
        )
        // Drop zone
        .onDrop(of: [.fileURL], isTargeted: $isFileTargeted) { providers in
            handleDrop(providers: providers)
        }
        .overlay(dropOverlay)
        // Notifications
        .onReceive(NotificationCenter.default.publisher(for: .openFilePicker)) { _ in
            model.openFilePicker()
        }
        .onAppear { model.refreshAll() }
        // Playlist window sync
        .onChange(of: model.playlistVisible) { _, visible in
            if visible { openWindow(id: "playlist") }
            else       { dismissWindow(id: "playlist") }
        }
        // Right-click / two-finger-tap context menu for theme switching
        .contextMenu {
            themeMenu
        }
    }

    // MARK: – LCD Panel (Winamp-style)
    // Left: large tappable time display. Right: marquee title.

    private var lcdPanel: some View {
        ZStack {
            theme.lcdBackground

            HStack(spacing: 0) {
                // ── Time display (tappable — toggles remaining/elapsed) ───────
                Button { model.toggleRemainingTime() } label: {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(timeDisplay)
                            .font(.system(size: 26, weight: .bold, design: .monospaced))
                            .foregroundStyle(theme.timeText)
                            .lineLimit(1)
                            .minimumScaleFactor(0.6)
                        Text(model.showRemainingTime ? "REMAIN" : "ELAPSED")
                            .font(.system(size: 7, weight: .medium))
                            .foregroundStyle(theme.timeText.opacity(0.5))
                    }
                    .padding(.horizontal, 10)
                    .padding(.vertical, 8)
                    .frame(width: 110, alignment: .leading)
                }
                .buttonStyle(.plain)
                .help("Click to toggle remaining/elapsed time")

                Divider()
                    .background(theme.lcdBorder)
                    .padding(.vertical, 6)

                // ── Track info: state icon + marquee title ───────────────────
                VStack(alignment: .leading, spacing: 3) {
                    HStack(spacing: 5) {
                        Image(systemName: stateIcon)
                            .font(.system(size: 9, weight: .bold))
                            .foregroundStyle(stateColor)
                            .frame(width: 12)
                        MarqueeView(text: marqueeText)
                            .frame(height: 16)
                    }
                    Text(artistText)
                        .font(.system(size: 10))
                        .foregroundStyle(theme.artistText)
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
                .padding(.horizontal, 10)
                .padding(.vertical, 8)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .frame(height: 62)
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
        .padding(.vertical, 5)
    }

    // MARK: – Transport row (prev ▶ ⏸ ⏹ next)

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
        .padding(.top, 5)
        .padding(.bottom, 2)
    }

    // MARK: – Mode row (repeat + shuffle, smaller, above the bottom row)

    private var modeRow: some View {
        HStack(spacing: 6) {
            ModeButton(label: repeatLabel, isActive: model.repeatMode != 0) { model.cycleRepeat() }
                .help("Cycle repeat (r)")
            ModeButton(icon: "shuffle", isActive: model.shuffleEnabled) { model.toggleShuffle() }
                .help("Toggle shuffle (s)")
            Spacer()
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 3)
    }

    // MARK: – Bottom row (volume + playlist toggle + logo)

    private var bottomRow: some View {
        HStack(spacing: 6) {
            Image(systemName: "speaker.fill")
                .font(.system(size: 9))
                .foregroundStyle(theme.volumeThumb.opacity(0.6))

            ThemedVolumeSlider(
                value: Binding(get: { model.volume }, set: { model.setVolume($0) })
            )

            Image(systemName: "speaker.wave.3.fill")
                .font(.system(size: 9))
                .foregroundStyle(theme.volumeThumb.opacity(0.6))

            Spacer()

            ModeButton(icon: "list.bullet", isActive: model.playlistVisible) {
                model.playlistVisible.toggle()
            }
            .help("Show/hide Playlist (p)")

            SparkampLogoView()
        }
        .padding(.horizontal, 10)
        .padding(.top, 3)
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

    private var marqueeText: String {
        if model.currentTitle.isEmpty { return "Sparkamp" }
        return model.currentTitle
    }

    private var artistText: String {
        model.currentArtist.isEmpty ? " " : model.currentArtist
    }

    /// Large time string shown in the LCD: elapsed or (negative) remaining.
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
            .frame(minWidth: 22, minHeight: 18)
            .padding(.horizontal, 4)
            .background(
                RoundedRectangle(cornerRadius: 3)
                    .fill(isActive
                          ? theme.modeBtnActiveBg
                          : isHovered ? theme.transportHoverBg : theme.modeBtnBg)
                    .overlay(
                        RoundedRectangle(cornerRadius: 3)
                            .stroke(isActive ? theme.modeBtnActiveText.opacity(0.4) : theme.modeBtnBorder,
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
        let trackH: CGFloat  = 4
        let thumbD: CGFloat  = isHovered || isDragging ? 13 : 9

        GeometryReader { geo in
            let W    = geo.size.width
            let midY = geo.size.height / 2
            let pad  = thumbD / 2
            let fillW = CGFloat(displayFraction) * (W - thumbD)
            let thumbX = pad + fillW

            ZStack(alignment: .leading) {
                // Track background
                Capsule()
                    .fill(t.seekTrack)
                    .frame(height: trackH)
                    .padding(.horizontal, pad)

                // Filled portion
                Capsule()
                    .fill(t.seekFill)
                    .frame(width: max(pad, fillW + pad), height: trackH)

                // Thumb
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
        let t = themeManager.currentTheme
        Slider(value: $value, in: 0...1)
            .tint(t.volumeThumb)
    }
}
