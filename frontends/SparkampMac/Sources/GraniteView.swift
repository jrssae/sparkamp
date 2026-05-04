import SwiftUI
import AppKit

// MARK: - Granite plasma visualizer (mini + fullscreen)

/// SwiftUI shim around an `NSImageView` driven at 30 fps. Each frame asks
/// the Rust core to fill an RGBA8 buffer with the Granite plasma, wraps
/// that buffer in a `CGImage`, and assigns it as the `NSImageView.image`.
///
/// `NSImageView` is `CALayer`-backed, so the upscale to display size is a
/// GPU bilinear blit handled by Quartz — keeping CPU cost on the granite
/// kernel itself (≈ 12 ms/frame at 640×360) regardless of viewport.
struct GraniteView: NSViewRepresentable {
    @EnvironmentObject var model: SparkampModel

    func makeCoordinator() -> Coordinator {
        Coordinator(model: model)
    }

    func makeNSView(context: Context) -> NSImageView {
        let v = NSImageView()
        v.imageScaling = .scaleAxesIndependently
        v.imageAlignment = .alignCenter
        v.wantsLayer = true
        // Solid background so the plasma sits on a known black canvas instead
        // of bleeding the SwiftUI view's transparency through during resize.
        v.layer?.backgroundColor = NSColor.black.cgColor

        // Drive at 30 fps via a Timer on the main run loop. Stored on the
        // coordinator so it lives exactly as long as the view.
        let timer = Timer.scheduledTimer(
            withTimeInterval: 1.0 / 30.0,
            repeats: true
        ) { [weak v] _ in
            guard let v else { return }
            context.coordinator.tick(into: v)
        }
        RunLoop.main.add(timer, forMode: .common)
        context.coordinator.timer = timer
        return v
    }

    func updateNSView(_ nsView: NSImageView, context: Context) {
        // Nothing to do; the timer pulls the latest `model.ctx` each tick.
    }

    static func dismantleNSView(_ nsView: NSImageView, coordinator: Coordinator) {
        coordinator.timer?.invalidate()
        coordinator.timer = nil
    }

    /// Per-instance state: the pixel buffer, last-seen dimensions, and the
    /// timer reference for cleanup.
    final class Coordinator {
        let model: SparkampModel
        var timer: Timer?
        private var buffer = [UInt8]()
        private var w: Int = 0
        private var h: Int = 0

        init(model: SparkampModel) {
            self.model = model
        }

        /// 30 fps tick: render one frame and present it.
        func tick(into view: NSImageView) {
            guard let ctx = model.ctx else { return }
            let viewSize = view.bounds.size
            // Aspect-matched internal resolution: short axis fixed at 360,
            // long axis derived from the view's aspect ratio. Matches the GTK
            // path via `sparkamp::granite::GRANITE_INTERNAL_HEIGHT`.
            let internalH = 360
            let aspect: Double
            if viewSize.height > 0 {
                aspect = max(0.5, min(4.0, Double(viewSize.width / viewSize.height)))
            } else {
                aspect = 16.0 / 9.0
            }
            let internalW = max(64, Int((Double(internalH) * aspect).rounded()))

            // Reallocate the buffer if dimensions changed.
            let need = internalW * internalH * 4
            if buffer.count != need {
                buffer = [UInt8](repeating: 0, count: need)
                w = internalW
                h = internalH
            }

            buffer.withUnsafeMutableBufferPointer { ptr in
                guard let base = ptr.baseAddress else { return }
                sparkamp_render_granite(
                    ctx,
                    base,
                    UInt32(internalW),
                    UInt32(internalH)
                )
            }

            guard let cgImage = makeCGImage(width: internalW, height: internalH) else {
                return
            }
            // NSImage from CGImage at 1× — Quartz handles the bilinear upscale.
            let nsImage = NSImage(cgImage: cgImage,
                                  size: NSSize(width: internalW, height: internalH))
            view.image = nsImage
        }

        /// Wrap `buffer` in a CGImage with RGBA8 → premultiplied-last layout.
        private func makeCGImage(width: Int, height: Int) -> CGImage? {
            let stride = width * 4
            let dataLen = stride * height
            return buffer.withUnsafeBufferPointer { ptr -> CGImage? in
                guard let base = ptr.baseAddress else { return nil }
                guard let provider = CGDataProvider(
                    dataInfo: nil,
                    data: base,
                    size: dataLen,
                    releaseData: { _, _, _ in }
                ) else {
                    return nil
                }
                let bitmapInfo = CGBitmapInfo(rawValue:
                    CGImageAlphaInfo.premultipliedLast.rawValue
                )
                return CGImage(
                    width: width,
                    height: height,
                    bitsPerComponent: 8,
                    bitsPerPixel: 32,
                    bytesPerRow: stride,
                    space: CGColorSpaceCreateDeviceRGB(),
                    bitmapInfo: bitmapInfo,
                    provider: provider,
                    decode: nil,
                    shouldInterpolate: true,
                    intent: .defaultIntent
                )
            }
        }
    }
}
