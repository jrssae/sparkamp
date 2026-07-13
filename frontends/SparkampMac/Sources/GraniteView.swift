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
        let coordinator = context.coordinator
        // Both instances tick on a fixed 30 fps main-run-loop Timer.
        // Display-rate pacing (CADisplayLink at 60/120 Hz) was tried and
        // REVERTED: several Granite effects glitch visibly whenever the
        // sim runs at fractional dt (≈0.5 at 60 Hz, jittering under load),
        // and the corruption compounds across fullscreen re-entries. Until
        // the core's delta-time path is fixed and verified at dt≠1, a
        // stable 30 beats a glitchy 60 — at 30 Hz the measured dt stays
        // ≈1.0, which reproduces the historical sim bit-for-bit.
        // A strict DispatchSourceTimer, not a run-loop Timer: NSTimer gets
        // system leeway/coalescing, which was observed delivering 20 of the
        // 30 requested ticks. `.strict` opts out of coalescing, and the
        // handler runs directly on the main queue (no actor-hop — routing
        // each tick through Task { @MainActor } also cost a third of them).
        let timer = DispatchSource.makeTimerSource(flags: .strict, queue: .main)
        timer.schedule(
            deadline: .now(),
            repeating: .milliseconds(33),
            leeway: .milliseconds(2)
        )
        timer.setEventHandler { [weak v] in
            guard let v else { return }
            MainActor.assumeIsolated { coordinator.tick(into: v) }
        }
        timer.activate()
        coordinator.timer = timer
        return v
    }

    func updateNSView(_ nsView: GraniteBlitView, context: Context) {
        // Nothing to do; the tick pulls the latest `model.ctx` each frame.
    }

    static func dismantleNSView(_ nsView: GraniteBlitView, coordinator: Coordinator) {
        coordinator.timer?.cancel()
        coordinator.timer = nil
    }

    /// Per-instance state: the pixel buffer and the tick-source references
    /// for cleanup. @MainActor because it reads `model.ctx`.
    @MainActor
    final class Coordinator {
        let model: SparkampModel
        let isFullscreen: Bool
        var timer: DispatchSourceTimer?
        private var buffer = [UInt8]()
        /// Previous render timestamp — measured dt keeps the sim's speed
        /// exact regardless of the tick source's real cadence.
        private var lastRender: Date?

        init(model: SparkampModel, isFullscreen: Bool) {
            self.model = model
            self.isFullscreen = isFullscreen
        }

        /// One tick: render one frame and present it.
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
            // Real presented-frame count for the fullscreen FPS overlay —
            // the overlay's own sampler runs at a fixed low rate and must
            // not measure itself. The render cost rides along so a low FPS
            // reading distinguishes callback overrun from system throttling.
            if isFullscreen {
                model.noteVizFrame()
                let ms = Date().timeIntervalSince(now) * 1000.0
                model.vizRenderMs = model.vizRenderMs == 0
                    ? ms : model.vizRenderMs * 0.9 + ms * 0.1
            }
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
