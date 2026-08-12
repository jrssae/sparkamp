import SwiftUI

// MARK: - Mini visualizer

/// Canvas-based frequency-bars or waveform view, polled at 30 fps via TimelineView.
///
/// Reads PCM / spectrum data directly from the Rust FFI context (via SparkampModel)
/// inside the Canvas draw closure — no @Published properties involved, so the
/// 30-fps updates never trigger a full SwiftUI layout pass.
///
/// Single-click cycles Bars → Waveform → Granite (Winamp behavior, matching
/// the GTK frontend's click controller); double-click opens the fullscreen
/// visualizer (Waveform or Granite mode).
struct VisualizerView: View {
    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager

    var body: some View {
        Group {
            // Branch on the published mirror, not a direct FFI read: a body
            // rebuild only happens when SwiftUI sees a dependency change, so
            // reading the FFI here would leave a stale branch after the mode
            // changes (the click below, the v key, or Settings).
            if model.vizMode == 2 {
                // Granite plasma: dedicated layer-blit view fed by the Rust core.
                GraniteView()
                    .background(Color.black)
            } else {
                TimelineView(.animation(minimumInterval: 1.0 / 30.0)) { _ in
                    Canvas { gctx, size in
                        guard let ctx = model.ctx else { return }
                        let mode = sparkamp_get_viz_mode(ctx)
                        if mode == 0 {
                            VisualizerRenderer.drawBars(gctx: gctx, size: size, ctx: ctx)
                        } else {
                            VisualizerRenderer.drawWaveform(gctx: gctx, size: size, ctx: ctx)
                        }
                    }
                }
                .background(themeManager.currentTheme.lcdBackground)
            }
        }
        .overlay(
            // AppKit click handling instead of SwiftUI TapGesture: a lone
            // click cycles the mode after the user's double-click interval
            // (system setting, ~0.3 s — noticeably snappier than SwiftUI's
            // gesture arbitration), and a double-click opens fullscreen
            // WITHOUT cycling first.
            VizClickCatcher(
                onSingleClick: { model.cycleVizMode() },
                onDoubleClick: { model.fullscreenVizVisible = true }
            )
        )
    }

}

// MARK: - Zero-delay click handling

/// Transparent AppKit overlay that turns raw `mouseDown` events into
/// single/double-click callbacks. A single click is committed only after
/// the user's double-click interval passes with no second click — so a
/// double-click opens fullscreen WITHOUT cycling the mode first. AppKit
/// keeps this cheaper than SwiftUI's TapGesture arbitration: the deadline
/// is `NSEvent.doubleClickInterval` (the user's own system setting,
/// typically ~0.3 s), not a fixed gesture-recognizer window.
private struct VizClickCatcher: NSViewRepresentable {
    let onSingleClick: () -> Void
    let onDoubleClick: () -> Void

    func makeNSView(context: Context) -> ClickView {
        let v = ClickView()
        v.onSingleClick = onSingleClick
        v.onDoubleClick = onDoubleClick
        return v
    }

    func updateNSView(_ nsView: ClickView, context: Context) {
        nsView.onSingleClick = onSingleClick
        nsView.onDoubleClick = onDoubleClick
    }

    final class ClickView: NSView {
        var onSingleClick: (() -> Void)?
        var onDoubleClick: (() -> Void)?
        /// Single-click action awaiting the double-click deadline; a second
        /// click cancels it and fires the double action instead.
        private var pendingSingle: DispatchWorkItem?

        // React on the click that focuses the window too (Winamp feel).
        override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
        override var acceptsFirstResponder: Bool { false }

        override func mouseDown(with event: NSEvent) {
            switch event.clickCount {
            case 1:
                let work = DispatchWorkItem { [weak self] in
                    self?.pendingSingle = nil
                    self?.onSingleClick?()
                }
                pendingSingle = work
                DispatchQueue.main.asyncAfter(
                    deadline: .now() + NSEvent.doubleClickInterval, execute: work)
            case 2:
                pendingSingle?.cancel()
                pendingSingle = nil
                onDoubleClick?()
            default:
                break
            }
        }
    }
}
