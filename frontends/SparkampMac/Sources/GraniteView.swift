import SwiftUI
import AppKit

// MARK: - Granite plasma visualizer (mini + fullscreen)

/// Layer-blit host for the Granite plasma. Each 30 fps tick asks the Rust
/// core to fill an RGBA8 buffer at a view-scaled internal resolution, wraps
/// it in a CGImage backed by copied bytes, and assigns it straight to the
/// backing layer's `contents` — the cheapest path to the GPU compositor
/// (no NSImage, no NSImageView layout machinery).
///
/// Exactly one instance drives the shared Rust renderer at a time: the mini
/// view stops ticking while the fullscreen visualizer is open, and any
/// instance whose window is occluded skips its tick. Two live views with
/// different aspect ratios would otherwise force a buffer-clearing
/// `resize()` + warp-map rebuild inside the Rust core every single frame —
/// the visible result is a black visualizer and a tanked framerate.
struct GraniteView: NSViewRepresentable {
    @EnvironmentObject var model: SparkampModel
    /// True for the instance inside FullscreenVisualizerView.
    var isFullscreen: Bool = false

    func makeCoordinator() -> Coordinator {
        Coordinator(model: model, isFullscreen: isFullscreen)
    }

    func makeNSView(context: Context) -> GraniteBlitView {
        let v = GraniteBlitView()
        // Drive at 30 fps via a Timer on the main run loop. Stored on the
        // coordinator so it lives exactly as long as the view. Hop onto the
        // main actor the same way SparkampModel's tick timer does.
        let coordinator = context.coordinator
        // Fullscreen runs at 60 fps (the dt-aware sim keeps the plasma's
        // speed identical); the windowed mini stays at 30 to save power.
        let fps: Double = coordinator.isFullscreen ? 60.0 : 30.0
        let timer = Timer.scheduledTimer(
            withTimeInterval: 1.0 / fps,
            repeats: true
        ) { [weak v] _ in
            Task { @MainActor in
                guard let v else { return }
                coordinator.tick(into: v)
            }
        }
        RunLoop.main.add(timer, forMode: .common)
        coordinator.timer = timer
        return v
    }

    func updateNSView(_ nsView: GraniteBlitView, context: Context) {
        // Nothing to do; the timer pulls the latest `model.ctx` each tick.
    }

    static func dismantleNSView(_ nsView: GraniteBlitView, coordinator: Coordinator) {
        coordinator.timer?.invalidate()
        coordinator.timer = nil
    }

    /// Per-instance state: the pixel buffer and the timer reference for
    /// cleanup. @MainActor because it reads `model.ctx`.
    @MainActor
    final class Coordinator {
        let model: SparkampModel
        let isFullscreen: Bool
        var timer: Timer?
        private var buffer = [UInt8]()
        /// Previous render timestamp — measured dt keeps the sim's speed
        /// exact regardless of the timer's real cadence.
        private var lastRender: Date?

        init(model: SparkampModel, isFullscreen: Bool) {
            self.model = model
            self.isFullscreen = isFullscreen
        }

        /// 30 fps tick: render one frame and present it.
        func tick(into view: GraniteBlitView) {
            guard let ctx = model.ctx else { return }
            // Single-driver rule (see type comment): the mini view yields
            // while the fullscreen window owns the renderer.
            if model.fullscreenVizVisible && !isFullscreen { return }
            // Skip entirely when our window can't be seen (occluded by the
            // fullscreen Space, miniaturized, hidden) — saves the whole
            // render and avoids fighting over the renderer's dimensions.
            guard let win = view.window,
                  win.occlusionState.contains(.visible) else { return }

            let viewSize = view.bounds.size
            guard viewSize.width > 1, viewSize.height > 1 else { return }

            // View-scaled internal resolution: the mini strip renders at its
            // own (retina) pixel height instead of a fixed 360 — an order of
            // magnitude fewer pixels. Fullscreen caps at the granite-internal
            // 360 and lets the GPU compositor upscale. Width is rounded down
            // to a multiple of 16 so live window resizes don't regenerate
            // the warp map on every single frame.
            let scale = min(win.backingScaleFactor, 2.0)
            let internalH = min(360, max(64, Int(viewSize.height * scale)))
            let aspect = max(0.5, min(4.0, Double(viewSize.width / viewSize.height)))
            var internalW = max(64, Int((Double(internalH) * aspect).rounded()))
            internalW &= ~15

            let need = internalW * internalH * 4
            if buffer.count != need {
                buffer = [UInt8](repeating: 0, count: need)
            }
            // Elapsed time in 30 fps frame units (1.0 = 33 ms).
            let now = Date()
            let dtFrames: Float
            if let prev = lastRender {
                dtFrames = Float(now.timeIntervalSince(prev) * 30.0)
            } else {
                dtFrames = 1.0
            }
            lastRender = now
            buffer.withUnsafeMutableBufferPointer { ptr in
                guard let base = ptr.baseAddress else { return }
                sparkamp_render_granite(
                    ctx,
                    base,
                    UInt32(internalW),
                    UInt32(internalH),
                    dtFrames
                )
            }
            view.present(buffer: buffer, width: internalW, height: internalH)
        }
    }
}

/// Plain layer-backed view whose contents are replaced each frame.
///
/// Deliberately reports no intrinsic content size: the plasma must never
/// influence layout (an NSImageView in this spot once grew the whole player
/// window to its image's pixel size). Also refuses focus so it can't draw
/// a focus ring over the visual.
final class GraniteBlitView: NSView {
    override var intrinsicContentSize: NSSize {
        NSSize(width: NSView.noIntrinsicMetric, height: NSView.noIntrinsicMetric)
    }
    override var acceptsFirstResponder: Bool { false }

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true
        layer?.backgroundColor = NSColor.black.cgColor
        layer?.magnificationFilter = .linear
        layer?.contentsGravity = .resize
        focusRingType = .none
    }

    required init?(coder: NSCoder) {
        fatalError("GraniteBlitView is never decoded from a nib")
    }

    /// Wrap the RGBA8 buffer in a CGImage and hand it to the backing layer.
    /// The bytes are copied (`Data(buffer)`) so the image never aliases the
    /// live render buffer the Rust core writes into on the next tick.
    func present(buffer: [UInt8], width: Int, height: Int) {
        let data = Data(buffer)
        guard let provider = CGDataProvider(data: data as CFData) else { return }
        guard let image = CGImage(
            width: width,
            height: height,
            bitsPerComponent: 8,
            bitsPerPixel: 32,
            bytesPerRow: width * 4,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue),
            provider: provider,
            decode: nil,
            shouldInterpolate: true,
            intent: .defaultIntent
        ) else { return }
        layer?.contents = image
    }
}
