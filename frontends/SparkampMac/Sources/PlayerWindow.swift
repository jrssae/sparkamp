import SwiftUI
import UniformTypeIdentifiers
import AppKit

// MARK: - Main player window

struct PlayerWindow: View {
    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager
    @Environment(\.openWindow)    var openWindow
    @Environment(\.dismissWindow) var dismissWindow

    @State private var isDraggingSeek = false
    @State private var seekPreview: Double = 0
    @State private var isFileTargeted = false
    @State private var volumeLabelOpacity: Double = 0
    @State private var volumeHideTask: DispatchWorkItem? = nil

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
        .onAppear { handleAppear() }
        .modifier(WindowManagerModifier(model: model, openWindow: openWindow, dismissWindow: dismissWindow))
        .onChange(of: model.playerExpanded) { _, _ in
            // .windowResizability(.contentSize) grows the player window when the
            // A1 panel appears but does NOT shrink it back on collapse — the
            // greedy Spacer + maxHeight:.infinity visualizer keep the fitting
            // height at the larger value. Force the window back to its content's
            // fitting size on every toggle (mirrors GTK's manual resize on
            // panel toggle). Deferred one runloop so SwiftUI lays out the new
            // collapsed/expanded content first.
            DispatchQueue.main.async {
                guard let win = NSApp.windows.first(where: { $0.title == "Sparkamp" }),
                      let cv = win.contentView else { return }
                cv.layoutSubtreeIfNeeded()
                win.setContentSize(cv.fittingSize)
            }
        }
    }

    private func handleAppear() {
        model.refreshAll()
        // onChange only fires on transitions, not on the initial value.
        // Open any windows whose state was restored as true from UserDefaults.
        if model.playlistVisible          { openWindow(id: "playlist") }
        if model.keyboardShortcutsVisible { openWindow(id: "shortcuts") }
        if model.equalizerVisible         { openWindow(id: "equalizer") }
        if model.mediaLibraryVisible      {
            model.openMediaLibrary()
            openWindow(id: "media-library")
        }
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
        let vars = themeManager.currentVars
        // LCD background is applied per-section, NOT to the whole panel:
        //   - Left column (time + visualizer): LCD bg the full column height.
        //   - Right column marquee row only:   LCD bg as a tight strip behind
        //                                      the marquee text only.
        //   - Right column volume/buttons row: no LCD bg — theme.background
        //                                      (from the body VStack) shows
        //                                      through, matching the bottom
        //                                      transport row.
        return HStack(spacing: 0) {

            // ── Left column: time + mini visualizer ──────────────────────
            VStack(spacing: 0) {
                // Time display (tappable)
                Button { model.toggleRemainingTime() } label: {
                    HStack(alignment: .center, spacing: timeRowSpacing) {
                        Image(systemName: stateIcon)
                            .font(.system(size: stateGlyphSize))
                            .foregroundStyle(stateColor)
                            // GTK reserves two character widths for this glyph
                            // (`width_chars(2)`) and centres it, which is what
                            // leaves the badge a cell of its own to sit in,
                            // clear of the glyph rather than overlapping it.
                            .frame(width: stateGlyphSlot)
                            .overlay(alignment: .bottomTrailing) {
                                // Stop-after-current (phase 6): stop-square on
                                // the play/pause/stop indicator next to the time
                                // index while armed (not on the play button).
                                if model.stopAfterCurrent {
                                    Image(systemName: "stop.fill")
                                        .font(.system(size: stateBadgeSize))
                                        .foregroundStyle(stateColor)
                                }
                            }
                        Text(timeDisplay)
                            .font(vars.largeMonospaceFont)
                            .foregroundStyle(theme.timeText)
                            .lineLimit(1)
                            .minimumScaleFactor(0.5)
                            // Six characters wide and centred, matching GTK's
                            // `width_chars(6)`, so the slot does not resize as
                            // the digits change. Deliberately NOT `.fixedSize()`:
                            // that made the text ignore the width it was offered
                            // and overflow the column instead of scaling, which
                            // clipped the leading "-" on remaining times.
                            .frame(width: timeSlot)
                    }
                    .padding(.top, 8)
                    .padding(.leading, timeRowLeading)
                    .padding(.bottom, 4)
                }
                .buttonStyle(.plain)
                .help("Click to toggle remaining / elapsed time")

                // Mini visualizer — fills the column minus leading padding.
                // .clipped() on the column prevents any overflow into the divider.
                VisualizerView()
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .padding(.leading, timeRowLeading)
                    .padding(.bottom, 6)
            }
            .frame(width: lcdColumnWidth)
            .background(theme.lcdBackground)
            .clipped()

            // ── Divider — full info-panel height ─────────────────────────
            Rectangle()
                .fill(theme.lcdBorder)
                .frame(width: 1)

            // ── Right column: song info + vol/mode controls ───────────────
            VStack(alignment: .leading, spacing: 0) {

                // Row 1 — scrolling "Artist — Title" on a 42 px LCD strip.
                //
                // Strip is sized so the marquee text's natural vertical
                // centering produces equal breathing room above and below
                // the text — bg above ≈ bg below ≈ 13 px (text glyphs are
                // ~16 px tall in the marquee font).
                //
                // .padding(.top, 1) AFTER .background shifts the entire
                // LCD-bg strip down to y=1 of the right column, keeping
                // the bg out of the translucent native title-bar zone and
                // visually aligning the text with the time digits in the
                // left column.
                //
                // Double-click opens the ID3 tag editor for the current track.
                // A small borderless arrow at the right end toggles the A1
                // now-playing panel below (mirrors the GTK inline marquee
                // arrow) — same action as the `w` key.
                // Marquee fills the LCD strip; a 22 px trailing reserve keeps
                // its clipped text clear of the chevron that overlays the
                // strip's right end. (Marquee + chevron were siblings with only
                // 4 px between them, so scrolling text ran right up under the
                // button.) Mirrors GTK's narrower marquee display area, but
                // reserving just the button's footprint so text isn't cut short.
                // The chevron toggles the A1 panel — same action as `w`.
                MarqueeView(text: marqueeText)
                    .frame(height: 42)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.trailing, 22)
                    .gesture(TapGesture(count: 2).onEnded {
                        model.openId3Editor()
                    })
                    .overlay(alignment: .trailing) {
                        Button {
                            model.playerExpanded.toggle()
                            model.saveState()
                        } label: {
                            Image(systemName: model.playerExpanded ? "chevron.up" : "chevron.down")
                                .font(.system(size: 10, weight: .semibold))
                                .foregroundStyle(theme.modeBtnText)
                                .frame(width: 18, height: 18)
                                // Opaque LCD-colored chip + outline so the button
                                // stays legible even when long marquee text
                                // scrolls behind it — a transparent chevron
                                // blended illegibly with the moving text.
                                .background(theme.lcdBackground)
                                .overlay(
                                    RoundedRectangle(cornerRadius: 3)
                                        .stroke(theme.lcdBorder, lineWidth: 1)
                                )
                        }
                        .buttonStyle(.plain)
                        .help("Show/hide now-playing panel (w)")
                    }
                    .padding(.horizontal, 10)
                    .background(theme.lcdBackground)
                    .padding(.top, 1)

                // A1 — expandable now-playing panel: persistent marquee above
                // (unchanged), art + auto-cycling tag/tech/stats/links
                // carousel below when expanded. Collapsed = today's layout
                // unchanged (nothing rendered here).
                if model.playerExpanded {
                    NowPlayingPanel(info: model.nowPlaying, trackKey: model.currentIndex)
                        .padding(.horizontal, 10)
                        .padding(.top, 6)
                        .padding(.bottom, 4)
                }

                Spacer(minLength: 0)

                // Row 2 — volume slider + ℹ + playlist (theme.background)
                HStack(spacing: 6) {
                        Image(systemName: "speaker.fill")
                            .font(.system(size: 9))
                            .foregroundStyle(theme.volumeThumb.opacity(0.7))

                        ThemedVolumeSlider(
                            value: Binding(
                                get: { model.volume },
                                set: { newVol in
                                    model.setVolume(newVol)
                                    showVolumeLabel()
                                })
                        )
                        .frame(maxWidth: 140)
                        // Fade-out volume percentage, drawn over the right end
                        // of the track instead of sitting beside it. It is
                        // invisible except for a moment after a volume change,
                        // so reserving permanent width for it only narrowed the
                        // slider — and with the wider LCD column there is no
                        // longer slack in this row to spend that way.
                        .overlay(alignment: .trailing) {
                            Text("\(Int(model.volume * 100))%")
                                .font(vars.smallMonospaceFont)
                                .foregroundStyle(theme.transportText)
                                .opacity(volumeLabelOpacity)
                                .animation(.easeOut(duration: 0.3), value: volumeLabelOpacity)
                                .fixedSize()
                                .padding(.trailing, 2)
                                .allowsHitTesting(false)
                        }

                        Image(systemName: "speaker.wave.2.fill")
                            .font(.system(size: 9))
                            .foregroundStyle(theme.volumeThumb.opacity(0.7))

                        Spacer(minLength: 0)

                        ModeButton(icon: "info.circle", isActive: model.keyboardShortcutsVisible) {
                            model.keyboardShortcutsVisible.toggle()
                        }
                        .help("Keyboard shortcuts (i)")

                        ModeButton(icon: "magnifyingglass", isActive: model.jumpToTrackVisible) {
                            model.jumpToTrackVisible.toggle()
                        }
                        .help("Jump to Track (j)")

                        ModeButton(icon: "music.note.house", isActive: model.mediaLibraryVisible) {
                            if model.mediaLibraryVisible {
                                model.mediaLibraryVisible = false
                            } else {
                                model.openMediaLibrary()
                            }
                        }
                        .help("Media Library (⌘L)")

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
                    .padding(.horizontal, 10)
                    .padding(.vertical, 8)
                }
                .frame(maxWidth: .infinity)
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

    // MARK: – Time-row metrics
    //
    // The LCD column is laid out from GTK's numbers rather than fixed points,
    // because both frontends drive it from the same skin variable. GTK's
    // `.time-disp` class is on the *row box*, so its state glyph and its time
    // digits both inherit `font-size: font_size_large` in a monospace family;
    // the glyph reserves 2 characters (`width_chars(2)`), the digits 6. At the
    // default 32 that is a 39.6 pt slot and a 118.7 pt slot.
    //
    // Deriving these from `fontSizeLarge` rather than hardcoding 40/119 keeps
    // a skin that changes `--sp-font-size-large` from overflowing the column.

    /// Point size for the play/pause/stop glyph, and for the badge on it.
    ///
    /// These are NOT GTK's nominal 32 px and 16 px, because copying those
    /// numbers across gives a much bigger indicator than GTK draws. GTK's glyph
    /// is *text* — `▶ ⏸ ⏹` inheriting the row's font size — and a text glyph
    /// inks only part of its em box, while an SF Symbol inks nearly all of it.
    /// Measured at font-size 32, against the 23.6 pt ink height of the digits
    /// beside them:
    ///
    ///     ▶ 19.4 (0.82×)   ⏸ ⏹ 15.6 (0.66×)   badge ⏹ @16 → 7.8 (0.33×)
    ///
    /// SF Symbols at 32 pt ink 25.5–26.5, about 60 % over. At 20 pt they ink
    /// 16.0–16.5 and the badge at 10 pt inks 8.0, which lands on GTK's figures
    /// and keeps GTK's exact half-size relationship between the two.
    private var stateGlyphSize: CGFloat { themeManager.currentVars.fontSizeLarge * 0.625 }
    private var stateBadgeSize: CGFloat { stateGlyphSize / 2 }

    /// Advance width of one monospaced digit at the LCD font size.
    private var timeCharWidth: CGFloat {
        let font = NSFont.monospacedSystemFont(ofSize: themeManager.currentVars.fontSizeLarge,
                                               weight: .regular)
        return ("0" as NSString).size(withAttributes: [.font: font]).width
    }

    /// Two characters, matching GTK's `width_chars(2)` on the state label.
    private var stateGlyphSlot: CGFloat { (timeCharWidth * 2).rounded(.up) }

    /// Five characters, where GTK reserves six.
    ///
    /// The glyph slot above is what actually costs this column its width, and
    /// this window is a fixed 480 pt — six characters here would leave the
    /// volume slider at roughly half its length. Five holds every elapsed time
    /// and any remaining time under ten minutes at full size; longer ones
    /// (`-12:34`) scale down a few points via `minimumScaleFactor` rather than
    /// widening the column.
    private var timeSlot: CGFloat { (timeCharWidth * 5).rounded(.up) }

    /// Gap between the glyph and the digits — GTK's `time_row` box spacing.
    private let timeRowSpacing: CGFloat = 4
    /// Inset shared by the time row and the mini visualizer below it.
    private let timeRowLeading: CGFloat = 10

    /// Width of the whole LCD column: the inset, both slots, the gap between
    /// them, and a little trailing air so the digits never touch the divider.
    private var lcdColumnWidth: CGFloat {
        timeRowLeading + stateGlyphSlot + timeRowSpacing + timeSlot + 5
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

    private func showVolumeLabel() {
        volumeHideTask?.cancel()
        volumeLabelOpacity = 1
        let task = DispatchWorkItem {
            withAnimation { volumeLabelOpacity = 0 }
        }
        volumeHideTask = task
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.0, execute: task)
    }

    private func handleDrop(providers: [NSItemProvider]) -> Bool {
        // Accept both Sparkamp's internal tracklist UTI (multi-row drag
        // from any list) and plain file URLs (Finder, single-row drag).
        TrackDragPayload.resolvePaths(from: providers) { paths in
            guard !paths.isEmpty else { return }
            model.addFiles(paths.map { URL(fileURLWithPath: $0) })
        }
        return true
    }
}

// MARK: - Window manager modifier

private struct WindowManagerModifier: ViewModifier {
    @ObservedObject var model: SparkampModel
    let openWindow: OpenWindowAction
    let dismissWindow: DismissWindowAction

    func body(content: Content) -> some View {
        content
            .onChange(of: model.playlistVisible)          { _, v in v ? openWindow(id: "playlist")         : dismissWindow(id: "playlist") }
            .onChange(of: model.keyboardShortcutsVisible) { _, v in v ? openWindow(id: "shortcuts")         : dismissWindow(id: "shortcuts") }
            .onChange(of: model.fullscreenVizVisible)     { _, v in
                if v {
                    openWindow(id: "fullscreen-viz")
                } else {
                    dismissWindow(id: "fullscreen-viz")
                    // AppKit hands key status to an arbitrary surviving window
                    // (often the Media Library) when the fullscreen Space
                    // closes; the player should come back instead. Next
                    // runloop turn, after the dismissal has settled.
                    DispatchQueue.main.async {
                        NSApp.windows.first { $0.title == "Sparkamp" }?.makeKeyAndOrderFront(nil)
                    }
                }
            }
            .onChange(of: model.jumpToTrackVisible)       { _, v in v ? openWindow(id: "jump-to-track")     : dismissWindow(id: "jump-to-track") }
            .onChange(of: model.equalizerVisible)         { _, v in v ? openWindow(id: "equalizer")         : dismissWindow(id: "equalizer") }
            .onChange(of: model.settingsVisible)          { _, v in v ? openWindow(id: "settings")          : dismissWindow(id: "settings") }
            .onChange(of: model.id3EditorVisible)         { _, v in if !v { dismissWindow(id: "id3-editor") } }
            // Open request bumps on every "edit tags" action; openWindow on a
            // unique Window raises the existing editor (or creates it) — so a
            // second file selection reuses + fronts the window.
            .onChange(of: model.id3Request)               { _, _ in openWindow(id: "id3-editor") }
            .onChange(of: model.lyricsVisible)            { _, v in if !v { dismissWindow(id: "lyrics-viewer") } }
            // Same open-or-raise idiom as id3Request: bumped on every lyrics view.
            .onChange(of: model.lyricsRequest)            { _, _ in openWindow(id: "lyrics-viewer") }
            .onChange(of: model.artworkWindowVisible)     { _, v in v ? openWindow(id: "artwork")           : dismissWindow(id: "artwork") }
            // A6 open-or-focus: bumped on every "k" / art-tap / View-Art
            // request so a repeat press re-fronts the already-open singleton
            // (openWindow on a unique Window raises the existing instance —
            // same idiom as id3Request just below).
            .onChange(of: model.artworkWindowRequest)     { _, _ in openWindow(id: "artwork") }
            .onChange(of: model.mediaLibraryVisible)      { _, v in v ? openWindow(id: "media-library")     : dismissWindow(id: "media-library") }
            .onChange(of: model.dedupVisible)             { _, v in v ? openWindow(id: "deduplicator")      : dismissWindow(id: "deduplicator") }
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
        let vars = themeManager.currentVars
        return Button(action: action) {
            HStack(spacing: 3) {
                if let icon {
                    Image(systemName: icon)
                        .font(.system(size: 10, weight: .medium))
                }
                if let label {
                    Text(label)
                        .font(vars.bodyFont)
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

// MARK: - A1 now-playing panel (art + auto-cycling tag/tech/stats/links carousel)

/// Expanded content of the A1 now-playing panel: album art (clamped ~100pt)
/// on the left, and on the right a page of data that auto-advances every 6 s
/// (mirrors the GTK carousel's `CAROUSEL_INTERVAL` / `ROWS_PER_TAG_PAGE`),
/// with clickable page dots. Tapping the art opens the A6 album-art window
/// (follow-the-track mode). `trackKey` (the model's `currentIndex`) resets
/// the page back to 0 on every track change, same as GTK's `populate()`
/// resetting `c.index = 0`.
///
/// SIMPLIFICATION vs GTK: a manually-clicked dot does not push out the next
/// auto-advance (GTK's `jump()` doubles the dwell so a manual pick lingers);
/// here the timer just keeps advancing on its fixed schedule. Noted in the
/// mac checklist as a UX item to eyeball, not a correctness bug.
private struct NowPlayingPanel: View {
    let info: NowPlayingInfo?
    let trackKey: Int

    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager
    @State private var pageIndex: Int = 0
    @State private var artworkImage: NSImage? = nil

    private var theme: SkinTheme { themeManager.currentTheme }

    private enum Page {
        case tags([(String, String)])
        case tech(String)
        case stats(count: Int64?, last: String?)
        case links(artist: String?, album: String?)
    }

    /// Rows per tag page — matches GTK's `ROWS_PER_TAG_PAGE` so a
    /// metadata-rich file paginates identically on both frontends.
    private let rowsPerTagPage = 4

    private var pages: [Page] {
        guard let info else { return [] }
        var result: [Page] = []
        var i = 0
        while i < info.tags.count {
            let end = min(i + rowsPerTagPage, info.tags.count)
            result.append(.tags(Array(info.tags[i..<end])))
            i = end
        }
        if !info.techLine.isEmpty {
            result.append(.tech(info.techLine))
        }
        if info.hasPlayCount || !info.lastPlayed.isEmpty {
            result.append(.stats(
                count: info.hasPlayCount ? info.playCount : nil,
                last: info.lastPlayed.isEmpty ? nil : info.lastPlayed
            ))
        }
        if !info.artistWikiURL.isEmpty || !info.albumWikiURL.isEmpty {
            result.append(.links(
                artist: info.artistWikiURL.isEmpty ? nil : info.artistWikiURL,
                album: info.albumWikiURL.isEmpty ? nil : info.albumWikiURL
            ))
        }
        return result
    }

    /// Index of the LAST ID3/tags page (tag pages are the leading `.tags`
    /// entries). The "Lyrics" button lives only there (F15 revision, point 1).
    private var lastTagPageIndex: Int {
        var n = 0
        for p in pages {
            if case .tags = p { n += 1 } else { break }
        }
        return max(0, n - 1)
    }

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            artView
            VStack(alignment: .leading, spacing: 4) {
                pageContent
                    .frame(maxWidth: .infinity, minHeight: 60, alignment: .topLeading)
                // A1 lyrics affordance (F15, point 1): shown only on the LAST
                // ID3 page, opening the window in Current (follow-playback) mode.
                if pageIndex == lastTagPageIndex {
                    Button("Lyrics") {
                        model.viewOrSearchLyricsForPlaylist(index: trackKey, mode: .current)
                    }
                    .buttonStyle(.link)
                    .font(.system(size: 11))
                    // Indent past the label column so the button lines up with
                    // the values in the tag rows above it rather than with
                    // their labels.
                    .padding(.leading, Self.tagLabelWidth + Self.tagLabelSpacing)
                }
                if pages.count > 1 { dots }
            }
        }
        .onChange(of: trackKey) { _, _ in pageIndex = 0 }
        .onChange(of: pages.count) { _, count in
            if pageIndex >= count { pageIndex = 0 }
        }
        .onReceive(Timer.publish(every: 6, on: .main, in: .common).autoconnect()) { _ in
            guard pages.count > 1 else { return }
            pageIndex = (pageIndex + 1) % pages.count
        }
        // Reload the decoded image only when the path actually changes
        // (not on every 6 s page-advance re-render) — avoids re-hitting disk
        // on a timer.
        .task(id: info?.artworkPath ?? "") {
            let path = info?.artworkPath ?? ""
            artworkImage = path.isEmpty ? nil : NSImage(contentsOfFile: path)
        }
    }

    @ViewBuilder
    private var artView: some View {
        Group {
            if let img = artworkImage {
                Image(nsImage: img)
                    .resizable()
                    .aspectRatio(contentMode: .fit)
                    .frame(width: 100, height: 100)
                    .clipShape(RoundedRectangle(cornerRadius: 4))
            } else {
                VStack(spacing: 4) {
                    Image(nsImage: NSApp.applicationIconImage)
                        .resizable()
                        .frame(width: 40, height: 40)
                        .opacity(0.5)
                    Text("No artwork available")
                        .font(.system(size: 9))
                        .foregroundStyle(theme.playlistDurationText)
                        .multilineTextAlignment(.center)
                        .frame(width: 90)
                }
                .frame(width: 100, height: 100)
            }
        }
        .contentShape(Rectangle())
        .onTapGesture {
            model.openArtworkWindow()  // A6 — open-or-focus, follow-the-track mode
        }
    }

    @ViewBuilder
    private var pageContent: some View {
        if pages.isEmpty {
            Text("No metadata available")
                .font(.system(size: 11))
                .foregroundStyle(theme.playlistDurationText)
        } else {
            let safeIndex = pageIndex < pages.count ? pageIndex : 0
            switch pages[safeIndex] {
            case .tags(let rows):
                VStack(alignment: .leading, spacing: 3) {
                    ForEach(rows, id: \.0) { row in tagRow(row.0, row.1) }
                }
            case .tech(let line):
                Text(line)
                    .font(.system(size: 11))
                    .foregroundStyle(theme.playlistText)
            case .stats(let count, let last):
                VStack(alignment: .leading, spacing: 3) {
                    if let count { tagRow("Play count", "\(count)") }
                    if let last { tagRow("Last played", Self.formatLastPlayed(last)) }
                }
            case .links(let artist, let album):
                VStack(alignment: .leading, spacing: 3) {
                    if let artist, let url = URL(string: artist) {
                        Link("Artist on Wikipedia", destination: url)
                            .font(.system(size: 11))
                    }
                    if let album, let url = URL(string: album) {
                        Link("Album on Wikipedia", destination: url)
                            .font(.system(size: 11))
                    }
                }
            }
        }
    }

    /// Width of the label column in `tagRow`, shared with the Lyrics button
    /// so the button stays flush with the value column if this ever changes.
    private static let tagLabelWidth: CGFloat = 80
    private static let tagLabelSpacing: CGFloat = 6

    private func tagRow(_ label: String, _ value: String) -> some View {
        HStack(alignment: .top, spacing: Self.tagLabelSpacing) {
            Text("\(label):")
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(theme.playlistDurationText)
                .frame(width: Self.tagLabelWidth, alignment: .leading)
            Text(value)
                .font(.system(size: 11))
                .foregroundStyle(theme.playlistText)
                .lineLimit(2)
        }
    }

    @ViewBuilder
    private var dots: some View {
        HStack(spacing: 4) {
            ForEach(0..<pages.count, id: \.self) { i in
                Circle()
                    .fill(i == pageIndex ? theme.vars.highlight : theme.playlistDurationText.opacity(0.4))
                    .frame(width: 5, height: 5)
                    .contentShape(Rectangle())
                    .onTapGesture { pageIndex = i }
            }
        }
    }

    /// "yyyy-MM-dd HH:mm" local rendering of an ISO-8601 UTC timestamp —
    /// same pattern as `MLTrack.lastPlayedDisplay`.
    private static func formatLastPlayed(_ iso: String) -> String {
        let inFmt = ISO8601DateFormatter()
        guard let date = inFmt.date(from: iso) else { return iso }
        let outFmt = DateFormatter()
        outFmt.dateFormat = "yyyy-MM-dd HH:mm"
        return outFmt.string(from: date)
    }
}
