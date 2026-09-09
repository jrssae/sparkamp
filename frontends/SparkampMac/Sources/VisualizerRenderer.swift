import SwiftUI

// MARK: - Shared visualizer rendering

/// The bars and waveform renderers, shared by the mini visualizer
/// (`VisualizerView`) and the fullscreen one (`FullscreenVisualizerView`).
///
/// These lived twice, once in each view, and had drifted into two spellings of
/// the same arithmetic — same behaviour, different local names, different line
/// breaks, and the fullscreen copy had lost the comments explaining what the
/// zone maths is doing. Its header said "identical logic to VisualizerView",
/// which is an accurate description of a problem rather than a solution: any
/// fix to the zone bounds or the sample→y mapping had to be made twice, and
/// nothing would have failed if it were made once.
///
/// Every function here is a pure function of `(GraphicsContext, CGSize,
/// OpaquePointer)` — neither copy ever read a `@State`, the model or the theme,
/// which is what makes the shared home possible at all. Colours come from the
/// core per zone, so the two visualizers stay in step with the active skin
/// without either of them holding theme state.
enum VisualizerRenderer {

    // MARK: Bars renderer

    static func drawBars(gctx: GraphicsContext, size: CGSize, ctx: OpaquePointer) {
        let numBands   = Int(sparkamp_get_spectrum_bands(ctx))
        let numZones   = Int(sparkamp_get_viz_zones(ctx))
        let mirror     = sparkamp_get_viz_mirror(ctx)
        let zoneColors = barsZoneColors(ctx: ctx, numZones: numZones)

        var bands = [Float](repeating: 0, count: numBands)
        bands.withUnsafeMutableBufferPointer { ptr in
            sparkamp_get_spectrum(ctx, ptr.baseAddress, Int32(numBands))
        }

        var zonePaths = [Path](repeating: Path(), count: max(numZones, 1))
        let barW = size.width / CGFloat(numBands)
        for i in 0..<numBands {
            addZonedBar(
                to: &zonePaths,
                x: CGFloat(i) * barW,
                barW: barW,
                height: size.height,
                amp: CGFloat(bands[i]),
                mirror: mirror,
                numZones: numZones
            )
        }
        fillZones(zonePaths, colors: zoneColors, in: gctx)
    }

    /// Add one bar's rectangles to the per-zone paths.
    /// `mirror = true`: bar extends both above and below the center line.
    /// `mirror = false`: bar grows upward from the bottom of the view.
    ///
    /// This appends rather than fills because every rectangle in a zone shares
    /// that zone's colour, so the whole zone can go out as one fill. See
    /// `fillZones`.
    static func addZonedBar(
        to zonePaths: inout [Path],
        x: CGFloat, barW: CGFloat, height: CGFloat,
        amp: CGFloat, mirror: Bool,
        numZones: Int
    ) {
        let bw = barW - 0.75

        if mirror {
            let center    = height / 2.0
            let maxExtent = amp * center

            for zone in 0..<numZones {
                let zoneInner = CGFloat(zone)     * (center / CGFloat(numZones))
                let zoneOuter = CGFloat(zone + 1) * (center / CGFloat(numZones))

                if zoneOuter <= maxExtent {
                    zonePaths[zone].addRect(CGRect(x: x + 0.5, y: center + zoneInner,
                                                   width: bw, height: zoneOuter - zoneInner))
                    zonePaths[zone].addRect(CGRect(x: x + 0.5, y: center - zoneOuter,
                                                   width: bw, height: zoneOuter - zoneInner))
                } else if zoneInner < maxExtent {
                    let h = maxExtent - zoneInner
                    zonePaths[zone].addRect(CGRect(x: x + 0.5, y: center + zoneInner,
                                                   width: bw, height: h))
                    zonePaths[zone].addRect(CGRect(x: x + 0.5, y: center - maxExtent,
                                                   width: bw, height: h))
                }
            }
        } else {
            // Non-mirrored: bar grows upward from bottom
            let barH = amp * height
            let topY  = height - barH

            for zone in 0..<numZones {
                let zoneTopY = height - CGFloat(zone + 1) * (height / CGFloat(numZones))
                let zoneBotY = height - CGFloat(zone)     * (height / CGFloat(numZones))
                let drawTop  = max(topY,   zoneTopY)
                let drawBot  = min(height, zoneBotY)
                if drawTop < drawBot {
                    zonePaths[zone].addRect(CGRect(x: x + 0.5, y: drawTop,
                                                   width: bw, height: drawBot - drawTop))
                }
            }
        }
    }

    // MARK: Waveform renderer

    static func drawWaveform(gctx: GraphicsContext, size: CGSize, ctx: OpaquePointer) {
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

        var zonePaths = [Path](repeating: Path(), count: max(numZones, 1))

        if style == 0 {
            // Lines: every segment becomes a subpath of its zone's path, so
            // each zone is stroked once instead of once per sample.
            for i in 0..<(n - 1) {
                let x0 = CGFloat(i)     * width / CGFloat(n)
                let x1 = CGFloat(i + 1) * width / CGFloat(n)
                let y0 = ys[i]
                let y1 = ys[i + 1]
                let zone = zoneForY((y0 + y1) / 2.0, height: height, numZones: numZones)
                zonePaths[zone].move(to: CGPoint(x: x0, y: y0))
                zonePaths[zone].addLine(to: CGPoint(x: x1, y: y1))
            }
            for zone in zonePaths.indices where !zonePaths[zone].isEmpty {
                gctx.stroke(zonePaths[zone],
                            with: .color(zoneColors[min(zone, zoneColors.count - 1)]),
                            lineWidth: 1.5)
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
                        zonePaths[zone].addRect(CGRect(x: x, y: drawTop,
                                                       width: colW, height: drawBot - drawTop))
                    }
                }
            }
            fillZones(zonePaths, colors: zoneColors, in: gctx)
        }
    }

    // MARK: Helpers

    /// Fill each zone's accumulated path in one call.
    ///
    /// Both renderers used to issue a `Path` allocation and a `fill` per
    /// rectangle. Bars did up to `bands * zones * 2` of them, and the waveform
    /// one per sample, which at a fullscreen width is well over a thousand
    /// primitives a frame. Every rectangle in a zone shares that zone's colour,
    /// so they can all go into one path and out in one call. Measured over 363
    /// frames at 1680 samples and 5 zones: the waveform's line style fell from
    /// 2.773 ms to 0.539 ms a frame, bars from 0.112 ms to 0.042 ms, and that
    /// counts only the cost of recording the display list.
    static func fillZones(_ zonePaths: [Path], colors: [Color], in gctx: GraphicsContext) {
        for zone in zonePaths.indices where !zonePaths[zone].isEmpty {
            gctx.fill(zonePaths[zone], with: .color(colors[min(zone, colors.count - 1)]))
        }
    }

    static func zoneForY(_ y: CGFloat, height: CGFloat, numZones: Int) -> Int {
        let frac = (height - y) / height
        return min(Int(frac * CGFloat(numZones)), numZones - 1)
    }

    static func barsZoneColors(ctx: OpaquePointer, numZones: Int) -> [Color] {
        (0..<numZones).map { zone in
            let ptr = sparkamp_get_zone_color(ctx, Int32(zone))
            let hex = ptr.map { String(cString: $0) } ?? "#006600"
            sparkamp_free_string(ptr)
            return Color(hex: hex) ?? .green
        }
    }

    static func waveformZoneColors(ctx: OpaquePointer, numZones: Int) -> [Color] {
        (0..<numZones).map { zone in
            let ptr = sparkamp_get_waveform_zone_color(ctx, Int32(zone))
            let hex = ptr.map { String(cString: $0) } ?? "#006600"
            sparkamp_free_string(ptr)
            return Color(hex: hex) ?? .green
        }
    }
}
