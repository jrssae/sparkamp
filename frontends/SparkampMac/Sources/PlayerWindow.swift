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
            // ┌───────────────────────────────────────────────────────┐
            // │ [▶ TIME ]  │ Marquee title                            │
            // │ [viz 52px] │   [spacer]                               │
            // │            │ [🔊 vol≈140px 🔊🔊]  [ℹ] [PL]           │
            // ├───────────────────────────────────────────────────────┤
            // │ [seek bar — full width, thick]                        │
            // ├───────────────────────────────────────────────────────┤
            // │ [◀ ▶ ⏸ ⏹ ▶]  [Repeat][⇌ Shuffle]  [logo]          │
            // └───────────────────────────────────────────────────────┘
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
        .onAppear {
            model.refreshAll()
            // onChange only fires on transitions, not on the initial value.
            // Open any windows whose state was restored as true from UserDefaults.
            if model.playlistVisible          { openWindow(id: "playlist") }
            if model.keyboardShortcutsVisible { openWindow(id: "shortcuts") }
            if model.equalizerVisible         { openWindow(id: "equalizer") }
        }
        .onChange(of: model.playlistVisible) { _, visible in
            if visible { openWindow(id: "playlist") }
            else       { dismissWindow(id: "playlist") }
        }
        .onChange(of: model.keyboardShortcutsVisible) { _, visible in
            if visible { openWindow(id: "shortcuts") }
            else       { dismissWindow(id: "shortcuts") }
        }
        .onChange(of: model.fullscreenVizVisible) { _, visible in
            if visible { openWindow(id: "fullscreen-viz") }
            else       { dismissWindow(id: "fullscreen-viz") }
        }
        .onChange(of: model.jumpToTrackVisible) { _, visible in
            if visible { openWindow(id: "jump-to-track") }
            else       { dismissWindow(id: "jump-to-track") }
        }
        .onChange(of: model.equalizerVisible) { _, visible in
            if visible { openWindow(id: "equalizer") }
            else       { dismissWindow(id: "equalizer") }
        }
        .onChange(of: model.settingsVisible) { _, visible in
            if visible { openWindow(id: "settings") }
            else       { dismissWindow(id: "settings") }
        }
        .onChange(of: model.id3EditorVisible) { _, visible in
            if visible { openWindow(id: "id3-editor") }
            else       { dismissWindow(id: "id3-editor") }
        }
        .contextMenu { themeMenu }
    }

    // MARK: – Info Panel
    //
    // Left column  (118 px):
    //   Top:    [stateIcon] [large time]  ← tappable; toggles remaining/elapsed
    //   Bottom: [mini visualizer — bars or waveform, 52 px tall]
    //
    // Right column (fills rest):
    //   Row 1: Marquee "Artist — Title"
    //   Row 2: (Spacer — pushes vol row to bottom)
    //   Row 3: 🔊 [thin vol slider] 🔊🔊  [ℹ] [PL]

    private var infoPanel: some View {
        ZStack {
            theme.lcdBackground

            HStack(spacing: 0) {

                // ── Left column: time + mini visualizer ──────────────────────
                VStack(spacing: 0) {
                    // Time display (tappable)
                    Button { model.toggleRemainingTime() } label: {
                        HStack(alignment: .center, spacing: 4) {
                            Image(systemName: stateIcon)
                                .font(.system(size: 9, weight: .bold))
                                .foregroundStyle(stateColor)
                            Text(timeDisplay)
                                .font(.system(size: 28, weight: .bold, design: .monospaced))
                                .foregroundStyle(theme.timeText)
                                .lineLimit(1)
                                .minimumScaleFactor(0.5)
                                .fixedSize()
                        }
                        .frame(width: 118, alignment: .leading)
                        .padding(.top, 8)
                        .padding(.leading, 8)
                        .padding(.bottom, 4)
                    }
                    .buttonStyle(.plain)
                    .help("Click to toggle remaining / elapsed time")

                    // Mini visualizer — same width as left column
                    VisualizerView()
                        .frame(width: 118)
                        .frame(maxHeight: .infinity)
                        .padding(.bottom, 6)
                }
                .frame(width: 118)

                // ── Divider ──────────────────────────────────────────────────
                Rectangle()
                    .fill(theme.lcdBorder)
                    .frame(width: 1)
                    .padding(.vertical, 6)

                // ── Right column: song info + vol/mode controls ───────────────
                VStack(alignment: .leading, spacing: 0) {

                    // Row 1 — scrolling "Artist — Title"
                    MarqueeView(text: marqueeText)
                        .padding(.top, 2)

                    Spacer()

                    // Row 2 — volume slider + ℹ + playlist
                    HStack(spacing: 6) {
                        Image(systemName: "speaker.fill")
                            .font(.system(size: 9))
                            .foregroundStyle(theme.volumeThumb.opacity(0.7))

                        ThemedVolumeSlider(
                            value: Binding(get: { model.volume },
                                           set: { model.setVolume($0) })
                        )
                        .frame(maxWidth: 140)

                        Image(systemName: "speaker.wave.2.fill")
                            .font(.system(size: 9))
                            .foregroundStyle(theme.volumeThumb.opacity(0.7))

                        Spacer()

                        ModeButton(icon: "info.circle", isActive: model.keyboardShortcutsVisible) {
                            model.keyboardShortcutsVisible.toggle()
                        }
                        .help("Keyboard shortcuts (i)")

                        ModeButton(icon: "magnifyingglass", isActive: model.jumpToTrackVisible) {
                            model.jumpToTrackVisible.toggle()
                        }
                        .help("Jump to Track (j)")

                        ModeButton(icon: "slider.horizontal.3", isActive: model.equalizerVisible) {
                            model.equalizerVisible.toggle()
                            model.saveState()
                        }
                        .help("Equalizer (u)")

                        ModeButton(icon: "list.bullet", isActive: model.playlistVisible) {
                            model.playlistVisible.toggle()
                        }
                        .help("Show / hide Playlist (p)")
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

    // MARK: – Seek row (thick track)

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

    // MARK: – Transport row

    private var transportRow: some View {
        HStack(spacing: 8) {
            // ── Playback controls ───────────────────────────────────────────
            SkinButton(id: "prev",  icon: "backward.end.fill",  iconSize: 14) { model.prev() }
            SkinButton(id: "play",  icon: "play.fill",          iconSize: 16,
                       isHighlighted: model.isPlaying)  { model.play() }
            SkinButton(id: "pause", icon: "pause.fill",         iconSize: 14,
                       isHighlighted: model.isPaused)   { model.pause() }
            SkinButton(id: "stop",  icon: "stop.fill",          iconSize: 14) { model.stop() }
            SkinButton(id: "next",  icon: "forward.end.fill",   iconSize: 14) { model.next() }

            Spacer()

            // ── Repeat / Shuffle ─────────────────────────────────────────────
            ModeButton(label: repeatLabel, isActive: model.repeatMode != 0) {
                model.cycleRepeat()
            }
            .help("Cycle repeat (r)")

            ModeButton(label: "Shuffle", icon: "shuffle", isActive: model.shuffleEnabled) {
                model.toggleShuffle()
            }
            .help("Toggle shuffle (s)")

            Spacer()

            // ── App icon logo — click to open Settings ──────────────────────
            Image(nsImage: NSApp.applicationIconImage)
                .resizable()
                .interpolation(.high)
                .frame(width: 42, height: 42)
                .cornerRadius(8)
                .help("Sparkamp — click for Settings")
                .onTapGesture { model.settingsVisible.toggle() }
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

    // MARK: – Context menu

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
        if !model.currentArtist.isEmpty {
            return "\(model.currentArtist) — \(model.currentTitle)"
        }
        return model.currentTitle
    }

    private var timeDisplay: String {
        if model.showRemainingTime, model.duration > 0 {
            let remaining = max(0, model.duration - model.position)
            return "−" + formatDuration(remaining)
        }
        return formatDuration(model.position)
    }

    private var repeatLabel: String {
        switch model.repeatMode {
        case 1: return "Repeat 1"
        case 2: return "Repeat All"
        default: return "Repeat"
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

// MARK: - Mode button (repeat / shuffle / playlist / shortcuts / info)

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
            HStack(spacing: 3) {
                if let icon {
                    Image(systemName: icon)
                        .font(.system(size: 10, weight: .medium))
                }
                if let label {
                    Text(label)
                        .font(.system(size: 9, weight: .bold))
                }
            }
            .foregroundStyle(isActive ? theme.modeBtnActiveText : theme.modeBtnText)
            .frame(minHeight: 18)
            .padding(.horizontal, 6)
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

// MARK: - Themed seek bar (thick track)

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
        let trackH: CGFloat = 7                              // thick progress bar
        let thumbD: CGFloat = isHovered || isDragging ? 14 : 11

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
        .frame(height: 20)
    }
}

// MARK: - Themed volume slider (thin track — visually lighter than seek bar)

struct ThemedVolumeSlider: View {
    @Binding var value: Double
    @EnvironmentObject var themeManager: ThemeManager
    @State private var isHovered = false

    var body: some View {
        let t = themeManager.currentTheme
        let trackH: CGFloat = 3                              // thin volume track
        let thumbD: CGFloat = isHovered ? 10 : 7

        GeometryReader { geo in
            let W     = geo.size.width
            let midY  = geo.size.height / 2
            let pad   = thumbD / 2
            let fillW = CGFloat(value) * (W - thumbD)
            let thumbX = pad + fillW

            ZStack(alignment: .leading) {
                Capsule()
                    .fill(t.seekTrack)
                    .frame(height: trackH)
                    .padding(.horizontal, pad)

                Capsule()
                    .fill(t.volumeThumb)
                    .frame(width: max(pad, fillW + pad), height: trackH)

                Circle()
                    .fill(t.volumeThumb)
                    .frame(width: thumbD, height: thumbD)
                    .position(x: thumbX, y: midY)
                    .animation(.easeOut(duration: 0.08), value: thumbD)
            }
            .contentShape(Rectangle())
            .onHover { isHovered = $0 }
            .gesture(
                DragGesture(minimumDistance: 0)
                    .onChanged { v in
                        value = ((v.location.x - pad) / max(W - thumbD, 1))
                            .clamped(to: 0...1)
                    }
            )
        }
        .frame(height: 14)
    }
}
