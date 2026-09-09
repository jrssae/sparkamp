import SwiftUI

// MARK: - Frame rates

/// Repaint intervals for the Canvas visualizers, shared by the mini and
/// fullscreen views so the two cannot drift.
///
/// 60 Hz while playing. Granite runs on its own display link at the panel's
/// rate and has always looked smoother than these two for that reason; 30 Hz
/// was visibly steppier next to it. Idle drops to 1 Hz because `.periodic` is
/// a wall clock that would otherwise repaint a paused window forever.
enum VizRate {
    static let playing = 1.0 / 60.0
    static let idle    = 1.0
}

// MARK: - Mini visualizer

/// Canvas-based frequency-bars or waveform view, repainted at 30 fps on its
/// own periodic schedule.
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

    /// Fixed origin for the repaint schedule.
    ///
    /// `.periodic(from:)` takes the origin by value, so writing `.now` there
    /// re-anchors the schedule to the instant of every body rebuild, and a
    /// rebuilt schedule emits an entry immediately. Any `@State` write in the
    /// same body then feeds itself: write, rebuild, immediate entry, write.
    /// Measured in an isolated copy of this view's shape, a sampler writing
    /// @State ten times a second drove the visualizer to 259 ticks a second
    /// with `.now` and 61 with a fixed origin, the sampler itself 215 against
    /// 10. A `@State` origin is captured once and keeps the schedule on a
    /// stable phase.
    @State private var epoch = Date()

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
                // This TimelineView never drove the Canvas on its own. What
                // repainted bars and waveform was the model publishing
                // `position` 10 times a second: an ObservableObject's
                // invalidation marks the subtree dirty and skips the value
                // comparison that was otherwise throwing the redraw away. Move
                // the clock to its own publisher and both froze, while Granite
                // kept running because GraniteView blits its own layer. See
                // the note on `timeline.date` below for the actual mechanism.
                //
                // The rate follows playback because `.periodic` is a wall
                // clock and would otherwise repaint 30 times a second forever,
                // which is the idle work this branch set out to remove. Paused,
                // the spectrum data is frozen, so 1 Hz is enough to let it
                // settle.
                TimelineView(
                    .periodic(from: epoch, by: model.isPlaying ? VizRate.playing : VizRate.idle)
                ) { timeline in
                    Canvas { gctx, size in
                        // Reading the tick's date is what makes this redraw,
                        // and it is not dead code. SwiftUI decides whether to
                        // re-run a Canvas by comparing the values its draw
                        // closure captured, and everything else here captures
                        // `model`, whose pointer never changes. Capturing the
                        // date gives the comparison something that does.
                        //
                        // Measured: without this line the closure runs twice
                        // in three seconds. With it, 95 times.
                        _ = timeline.date
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
