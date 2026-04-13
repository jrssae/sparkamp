import SwiftUI

// MARK: - Mini visualizer

/// Canvas-based frequency-bars or waveform view, polled at 30 fps via TimelineView.
///
/// Reads PCM / spectrum data directly from the Rust FFI context (via SparkampModel)
/// inside the Canvas draw closure — no @Published properties involved, so the
/// 30-fps updates never trigger a full SwiftUI layout pass.
///
/// Double-click opens the fullscreen waveform visualizer (Waveform mode only).
struct VisualizerView: View {
    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager

    var body: some View {
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
        .background(themeManager.currentTheme.lcdBackground)
        .gesture(
            TapGesture(count: 2).onEnded {
                // Fullscreen only in waveform mode, matching GTK behavior.
                guard let ctx = model.ctx,
                      sparkamp_get_viz_mode(ctx) == 1 else { return }
                model.fullscreenVizVisible = true
            }
        )
    }

    // MARK: Bars renderer

    private func drawBars(gctx: GraphicsContext, size: CGSize, ctx: OpaquePointer) {
        let numBands  = Int(sparkamp_get_spectrum_bands(ctx))
        let numZones  = Int(sparkamp_get_viz_zones(ctx))
        let zoneColors = barsZoneColors(ctx: ctx, numZones: numZones)

        var bands = [Float](repeating: 0, count: numBands)
        bands.withUnsafeMutableBufferPointer { ptr in
            sparkamp_get_spectrum(ctx, ptr.baseAddress, Int32(numBands))
        }

        let barW = size.width / CGFloat(numBands)
        for i in 0..<numBands {
            drawZonedBar(
                gctx: gctx,
                x: CGFloat(i) * barW,
                barW: barW,
                height: size.height,
                amp: CGFloat(bands[i]),
                mirror: true,
                numZones: numZones,
                zoneColors: zoneColors
            )
        }
    }

    /// Draw a single bar with zone-based coloring, mirrored above/below center.
    private func drawZonedBar(
        gctx: GraphicsContext,
        x: CGFloat, barW: CGFloat, height: CGFloat,
        amp: CGFloat, mirror: Bool,
        numZones: Int, zoneColors: [Color]
    ) {
        let bw = barW - 0.75
        let center = height / 2.0
        let maxExtent = amp * center

        for zone in 0..<numZones {
            let zoneInner = CGFloat(zone)     * (center / CGFloat(numZones))
            let zoneOuter = CGFloat(zone + 1) * (center / CGFloat(numZones))
            let color = zoneColors[min(zone, zoneColors.count - 1)]

            if zoneOuter <= maxExtent {
                // Full zone segment
                gctx.fill(Path(CGRect(x: x + 0.5, y: center + zoneInner,
                                      width: bw, height: zoneOuter - zoneInner)),
                          with: .color(color))
                gctx.fill(Path(CGRect(x: x + 0.5, y: center - zoneOuter,
                                      width: bw, height: zoneOuter - zoneInner)),
                          with: .color(color))
            } else if zoneInner < maxExtent {
                // Partial zone segment
                let h = maxExtent - zoneInner
                gctx.fill(Path(CGRect(x: x + 0.5, y: center + zoneInner,
                                      width: bw, height: h)), with: .color(color))
                gctx.fill(Path(CGRect(x: x + 0.5, y: center - maxExtent,
                                      width: bw, height: h)), with: .color(color))
            }
        }
    }

    // MARK: Waveform renderer

    private func drawWaveform(gctx: GraphicsContext, size: CGSize, ctx: OpaquePointer) {
        let numZones   = Int(sparkamp_get_waveform_zones(ctx))
        let style      = Int(sparkamp_get_waveform_style(ctx))
        let sampleCount = max(Int(size.width), 64)
        let zoneColors = waveformZoneColors(ctx: ctx, numZones: numZones)

        var samples = [Float](repeating: 0, count: sampleCount)
        samples.withUnsafeMutableBufferPointer { ptr in
            sparkamp_get_waveform(ctx, ptr.baseAddress, Int32(sampleCount))
        }

        let width   = size.width
        let height  = size.height
        let centerY = height / 2.0

        // Dim centre baseline
        var baseline = Path()
        baseline.move(to: CGPoint(x: 0, y: centerY))
        baseline.addLine(to: CGPoint(x: width, y: centerY))
        gctx.stroke(baseline, with: .color(Color(red: 0, green: 0.2, blue: 0.08)),
                    lineWidth: 0.5)

        // sample ∈ [-1, 1] → y coordinate
        let ys: [CGFloat] = samples.map { s in
            (centerY - CGFloat(s) * centerY * 0.9).clamped(to: 0...height)
        }
        let n = sampleCount

        if style == 0 {
            // Lines: stroke each segment in its zone color
            for i in 0..<(n - 1) {
                let x0 = CGFloat(i)     * width / CGFloat(n)
                let x1 = CGFloat(i + 1) * width / CGFloat(n)
                let y0 = ys[i]
                let y1 = ys[i + 1]
                let zone  = zoneForY((y0 + y1) / 2.0, height: height, numZones: numZones)
                let color = zoneColors[min(zone, zoneColors.count - 1)]
                var seg = Path()
                seg.move(to: CGPoint(x: x0, y: y0))
                seg.addLine(to: CGPoint(x: x1, y: y1))
                gctx.stroke(seg, with: .color(color), lineWidth: 1.5)
            }
        } else {
            // Filled: fill column-by-column between waveform and centerline
            for i in 0..<n {
                let x    = CGFloat(i) * width / CGFloat(n)
                let colW = max(width / CGFloat(n), 1.0)
                let y    = ys[i]
                let (yTop, yBot): (CGFloat, CGFloat) =
                    y < centerY ? (y, centerY) : (centerY, y)
                for zone in 0..<numZones {
                    let zoneTopY = height - CGFloat(zone + 1) * height / CGFloat(numZones)
                    let zoneBotY = height - CGFloat(zone)     * height / CGFloat(numZones)
                    let drawTop = max(yTop, zoneTopY)
                    let drawBot = min(yBot, zoneBotY)
                    if drawTop < drawBot {
                        let color = zoneColors[min(zone, zoneColors.count - 1)]
                        gctx.fill(Path(CGRect(x: x, y: drawTop,
                                              width: colW, height: drawBot - drawTop)),
                                  with: .color(color))
                    }
                }
            }
        }
    }

    // MARK: Helpers

    private func zoneForY(_ y: CGFloat, height: CGFloat, numZones: Int) -> Int {
        let frac = (height - y) / height
        return min(Int(frac * CGFloat(numZones)), numZones - 1)
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
