import SwiftUI
import AppKit

// MARK: - Fullscreen visualizer window

/// Full-screen waveform or bars visualizer.
///
/// Opened via `f` key or double-click on the mini visualizer (Waveform mode).
/// Covers the entire display using OS-level fullscreen.
/// Keyboard passthrough: z x c v b r s i j all work as in the main window.
/// Esc exits fullscreen.
struct FullscreenVisualizerView: View {
    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager
    @Environment(\.dismiss) var dismiss

    @State private var toastMessage: String = ""
    @State private var toastVisible: Bool   = false

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()

            // Full-size visualizer canvas
            TimelineView(.animation(minimumInterval: 1.0 / 30.0)) { _ in
                Canvas { gctx, size in
                    guard let ctx = model.ctx else { return }
                    let mode = sparkamp_get_viz_mode(ctx)
                    if mode == 0 {
                        drawBars(gctx: gctx, size: size, ctx: ctx)
                    } else {
                        drawWaveform(gctx: gctx, size: size, ctx: ctx)
                    }
                }
            }
            .ignoresSafeArea()

            // Toast overlay
            if toastVisible {
                VStack {
                    Spacer()
                    Text(toastMessage)
                        .font(.system(size: 16, weight: .semibold))
                        .foregroundStyle(.white)
                        .padding(.horizontal, 20)
                        .padding(.vertical, 10)
                        .background(Color.black.opacity(0.7))
                        .clipShape(RoundedRectangle(cornerRadius: 8))
                        .padding(.bottom, 40)
                }
                .transition(.opacity)
                .animation(.easeInOut(duration: 0.3), value: toastVisible)
            }

            // Jump-to-track sheet triggered from this window
            if model.jumpToTrackVisible {
                JumpToTrackView()
                    .environmentObject(model)
                    .environmentObject(themeManager)
                    .frame(width: 500, height: 400)
                    .background(themeManager.currentTheme.background)
                    .clipShape(RoundedRectangle(cornerRadius: 10))
                    .shadow(radius: 20)
                    .transition(.opacity)
                    .animation(.easeInOut(duration: 0.15), value: model.jumpToTrackVisible)
            }
        }
        .onAppear {
            // Go OS-level fullscreen when the window appears.
            DispatchQueue.main.async {
                NSApp.windows
                    .first { $0.identifier?.rawValue == "fullscreen-viz" }?
                    .toggleFullScreen(nil)
            }
        }
        .onDisappear {
            model.fullscreenVizVisible = false
        }
        // Keyboard handling for the fullscreen window
        .onKeyPress(.escape) {
            closeFullscreen()
            return .handled
        }
        .onKeyPress("z") { model.prev();         showPlaybackToast(); return .handled }
        .onKeyPress("x") { model.play();         showPlaybackToast(); return .handled }
        .onKeyPress("c") { model.togglePlay();   showPlaybackToast(); return .handled }
        .onKeyPress("v") { model.stop();         showToast("Stopped");  return .handled }
        .onKeyPress("b") { model.next();         showPlaybackToast(); return .handled }
        .onKeyPress("r") { model.cycleRepeat();  showRepeatToast();   return .handled }
        .onKeyPress("s") { model.toggleShuffle(); showShuffleToast(); return .handled }
        .onKeyPress("i") {
            model.keyboardShortcutsVisible.toggle()
            return .handled
        }
        .onKeyPress("j") {
            model.jumpToTrackVisible.toggle()
            return .handled
        }
        .focusable()
        // Monitor state changes to show toasts automatically when triggered
        // by external events (next track auto-advance, etc.)
        .onChange(of: model.currentTitle) { _, title in
            if !title.isEmpty { showToast(title) }
        }
    }

    private func closeFullscreen() {
        // Exit OS fullscreen before closing so the window restores correctly.
        if let win = NSApp.windows.first(where: { $0.identifier?.rawValue == "fullscreen-viz" }),
           win.styleMask.contains(.fullScreen) {
            win.toggleFullScreen(nil)
        }
        model.fullscreenVizVisible = false
    }

    private func showToast(_ message: String) {
        toastMessage = message
        withAnimation { toastVisible = true }
        DispatchQueue.main.asyncAfter(deadline: .now() + 3) {
            withAnimation { toastVisible = false }
        }
    }

    private func showPlaybackToast() {
        let state: String
        if model.isPlaying    { state = "▶ \(model.currentArtist.isEmpty ? model.currentTitle : "\(model.currentArtist) — \(model.currentTitle)")" }
        else if model.isPaused { state = "⏸ Paused" }
        else                   { state = "⏹ Stopped" }
        showToast(state)
    }

    private func showRepeatToast() {
        let label: String
        switch model.repeatMode {
        case 1: label = "Repeat One"
        case 2: label = "Repeat All"
        default: label = "Repeat Off"
        }
        showToast(label)
    }

    private func showShuffleToast() {
        showToast(model.shuffleEnabled ? "Shuffle On" : "Shuffle Off")
    }

    // MARK: - Bars renderer (identical logic to VisualizerView)

    private func drawBars(gctx: GraphicsContext, size: CGSize, ctx: OpaquePointer) {
        let numBands   = Int(sparkamp_get_spectrum_bands(ctx))
        let numZones   = Int(sparkamp_get_viz_zones(ctx))
        let zoneColors = barsZoneColors(ctx: ctx, numZones: numZones)

        var bands = [Float](repeating: 0, count: numBands)
        bands.withUnsafeMutableBufferPointer { ptr in
            sparkamp_get_spectrum(ctx, ptr.baseAddress, Int32(numBands))
        }

        let barW = size.width / CGFloat(numBands)
        for i in 0..<numBands {
            drawZonedBar(gctx: gctx, x: CGFloat(i) * barW, barW: barW,
                         height: size.height, amp: CGFloat(bands[i]),
                         numZones: numZones, zoneColors: zoneColors)
        }
    }

    private func drawZonedBar(
        gctx: GraphicsContext,
        x: CGFloat, barW: CGFloat, height: CGFloat, amp: CGFloat,
        numZones: Int, zoneColors: [Color]
    ) {
        let bw     = barW - 0.75
        let center = height / 2.0
        let maxExt = amp * center

        for zone in 0..<numZones {
            let inner = CGFloat(zone)     * (center / CGFloat(numZones))
            let outer = CGFloat(zone + 1) * (center / CGFloat(numZones))
            let color = zoneColors[min(zone, zoneColors.count - 1)]

            if outer <= maxExt {
                gctx.fill(Path(CGRect(x: x + 0.5, y: center + inner, width: bw, height: outer - inner)), with: .color(color))
                gctx.fill(Path(CGRect(x: x + 0.5, y: center - outer, width: bw, height: outer - inner)), with: .color(color))
            } else if inner < maxExt {
                let h = maxExt - inner
                gctx.fill(Path(CGRect(x: x + 0.5, y: center + inner,  width: bw, height: h)), with: .color(color))
                gctx.fill(Path(CGRect(x: x + 0.5, y: center - maxExt, width: bw, height: h)), with: .color(color))
            }
        }
    }

    // MARK: - Waveform renderer (identical logic to VisualizerView)

    private func drawWaveform(gctx: GraphicsContext, size: CGSize, ctx: OpaquePointer) {
        let numZones    = Int(sparkamp_get_waveform_zones(ctx))
        let style       = Int(sparkamp_get_waveform_style(ctx))
        let sampleCount = max(Int(size.width), 64)
        let zoneColors  = waveformZoneColors(ctx: ctx, numZones: numZones)

        var samples = [Float](repeating: 0, count: sampleCount)
        samples.withUnsafeMutableBufferPointer { ptr in
            sparkamp_get_waveform(ctx, ptr.baseAddress, Int32(sampleCount))
        }

        let width   = size.width
        let height  = size.height
        let centerY = height / 2.0

        var baseline = Path()
        baseline.move(to: CGPoint(x: 0, y: centerY))
        baseline.addLine(to: CGPoint(x: width, y: centerY))
        gctx.stroke(baseline, with: .color(Color(red: 0, green: 0.2, blue: 0.08)), lineWidth: 0.5)

        let ys: [CGFloat] = samples.map { s in
            (centerY - CGFloat(s) * centerY * 0.9).clamped(to: 0...height)
        }
        let n = sampleCount

        if style == 0 {
            for i in 0..<(n - 1) {
                let x0    = CGFloat(i)     * width / CGFloat(n)
                let x1    = CGFloat(i + 1) * width / CGFloat(n)
                let y0    = ys[i];   let y1 = ys[i + 1]
                let zone  = zoneForY((y0 + y1) / 2.0, height: height, numZones: numZones)
                let color = zoneColors[min(zone, zoneColors.count - 1)]
                var seg = Path()
                seg.move(to: CGPoint(x: x0, y: y0))
                seg.addLine(to: CGPoint(x: x1, y: y1))
                gctx.stroke(seg, with: .color(color), lineWidth: 1.5)
            }
        } else {
            for i in 0..<n {
                let x    = CGFloat(i) * width / CGFloat(n)
                let colW = max(width / CGFloat(n), 1.0)
                let y    = ys[i]
                let (yTop, yBot): (CGFloat, CGFloat) = y < centerY ? (y, centerY) : (centerY, y)
                for zone in 0..<numZones {
                    let zTop = height - CGFloat(zone + 1) * height / CGFloat(numZones)
                    let zBot = height - CGFloat(zone)     * height / CGFloat(numZones)
                    let dTop = max(yTop, zTop);  let dBot = min(yBot, zBot)
                    if dTop < dBot {
                        gctx.fill(Path(CGRect(x: x, y: dTop, width: colW, height: dBot - dTop)),
                                  with: .color(zoneColors[min(zone, zoneColors.count - 1)]))
                    }
                }
            }
        }
    }

    // MARK: Helpers

    private func zoneForY(_ y: CGFloat, height: CGFloat, numZones: Int) -> Int {
        min(Int((height - y) / height * CGFloat(numZones)), numZones - 1)
    }

    private func barsZoneColors(ctx: OpaquePointer, numZones: Int) -> [Color] {
        (0..<numZones).map { zone in
            let ptr = sparkamp_get_zone_color(ctx, Int32(zone))
            let hex = ptr.map { String(cString: $0) } ?? "#006600"
            sparkamp_free_string(ptr)
            return Color(hex: hex) ?? .green
        }
    }

    private func waveformZoneColors(ctx: OpaquePointer, numZones: Int) -> [Color] {
        (0..<numZones).map { zone in
            let ptr = sparkamp_get_waveform_zone_color(ctx, Int32(zone))
            let hex = ptr.map { String(cString: $0) } ?? "#006600"
            sparkamp_free_string(ptr)
            return Color(hex: hex) ?? .green
        }
    }
}
