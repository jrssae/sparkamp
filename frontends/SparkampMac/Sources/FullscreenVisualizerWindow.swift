import SwiftUI
import AppKit

// MARK: - Window accessor

/// Bridges SwiftUI to AppKit to obtain the real NSWindow reference.
/// SwiftUI's WindowGroup does NOT set `window.identifier` to the group id,
/// so NSApp.windows lookup by identifier fails.
///
/// Using `viewDidMoveToWindow` instead of `DispatchQueue.main.async` is key:
/// the override fires synchronously on the same run-loop turn that the view
/// is inserted into the window, before the first layout/draw pass.  This lets
/// us set `alphaValue = 0` before the window becomes visible at its initial
/// size, eliminating the brief "wrong-size" flash before fullscreen entry.
private final class _WinHostView: NSView {
    var onWindow: ((NSWindow) -> Void)?
    private var fired = false

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        guard let w = window, !fired else { return }
        fired = true
        onWindow?(w)
    }
}

private struct WindowAccessor: NSViewRepresentable {
    var onWindow: (NSWindow) -> Void

    func makeNSView(context: Context) -> _WinHostView {
        let v = _WinHostView()
        v.onWindow = onWindow
        return v
    }

    func updateNSView(_ nsView: _WinHostView, context: Context) {}
}

// MARK: - Fullscreen visualizer window

/// Full-screen waveform or bars visualizer.
///
/// Opened via `f` key or double-click on the mini visualizer (Waveform or
/// Granite mode). Covers the entire display using OS-level fullscreen.
/// All keys are handled by the app-wide monitor (SparkampModel.handleRawKey):
/// transport keys work as in the main window, `g` toggles the FPS overlay,
/// `n` switches the Granite effect, `j` exits fullscreen then opens the jump
/// window, Esc exits. Window-opening keys (p i u d) are disabled — they
/// would open in the main Space and yank focus out of fullscreen.
struct FullscreenVisualizerView: View {
    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager

    @State private var hostWindow: NSWindow? = nil
    @State private var toastMessage: String  = ""
    @State private var toastVisible: Bool    = false
    /// Pending toast-hide; cancelled and rescheduled on each new toast so the
    /// dismiss timer restarts rather than the old one firing early.
    @State private var toastDismiss: DispatchWorkItem? = nil
    @State private var fpsValue: Double      = 0
    @State private var renderMsValue: Double = 0
    @State private var effectName: String    = ""
    @State private var bpmValue: Double      = 0
    @State private var meterValue: Int       = 0
    @State private var fpsLastTick: Date?    = nil
    /// model.vizFrameCount at the previous sample — the FPS reading is the
    /// frame-count delta over the wall-clock delta, so a low-rate sampler
    /// can report a 120 Hz render truthfully.
    @State private var fpsLastCount: UInt64  = 0
    @State private var fpsEma: Double        = 0

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()

            // Full-size visualizer. Granite uses the dedicated layer-blit path
            // so the GPU compositor handles upscaling at 4K; Bars / Waveform
            // stay on SwiftUI Canvas. Branch on the published mirror so a
            // mode change while fullscreen (v key) swaps the branch live.
            if model.vizMode == 2 {
                GraniteView(isFullscreen: true)
                    .ignoresSafeArea()
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
                        // Count the presented frame for the FPS overlay
                        // (plain var bump — never invalidates layout).
                        model.noteVizFrame()
                    }
                }
                .ignoresSafeArea()
            }

            // FPS + BPM overlay (top-right; toggled with `g` via the app-wide
            // key monitor — model.fullscreenFpsVisible, not local state).
            // BPM comes from the Granite beat detector; "--" until it locks.
            if model.fullscreenFpsVisible {
                VStack {
                    HStack {
                        Spacer()
                        Text(String(format: "%@   FPS: %.0f / %d   render: %.1f ms   BPM: %@%@",
                                    effectName,
                                    fpsValue,
                                    hostWindow?.screen?.maximumFramesPerSecond ?? 0,
                                    renderMsValue,
                                    bpmValue > 0 ? String(format: "%.0f", bpmValue) : "--",
                                    meterValue > 0 ? " (\(meterValue)/4)" : ""))
                            .font(.system(size: 14, weight: .semibold, design: .monospaced))
                            .foregroundStyle(.white)
                            .padding(.horizontal, 10)
                            .padding(.vertical, 6)
                            .background(Color.black.opacity(0.55))
                            .clipShape(RoundedRectangle(cornerRadius: 6))
                            .padding(.top, 16)
                            .padding(.trailing, 20)
                    }
                    Spacer()
                }
                .transition(.opacity)
            }

            // FPS sampler — a fixed low-rate ticker that reads the presented
            // frame COUNTER and reports Δframes/Δt. Timing the sampler's own
            // ticks (the old approach) capped the reading at the sampler's
            // rate and could never show the display-link's 60/120 Hz.
            TimelineView(.animation(minimumInterval: 1.0 / 10.0)) { ctx in
                Color.clear
                    .onChange(of: ctx.date) { _, now in
                        let count = model.vizFrameCount
                        if let prev = fpsLastTick {
                            let dt = now.timeIntervalSince(prev)
                            if dt > 0 {
                                // &- : the counter wraps by design.
                                let frames = Double(count &- fpsLastCount)
                                let inst = frames / dt
                                fpsEma = fpsEma == 0 ? inst : fpsEma * 0.8 + inst * 0.2
                                fpsValue = fpsEma
                            }
                        }
                        fpsLastTick = now
                        fpsLastCount = count
                        renderMsValue = model.vizRenderMs
                        if let c = model.ctx {
                            bpmValue = Double(sparkamp_get_granite_bpm(c))
                            meterValue = Int(sparkamp_get_granite_meter(c))
                            // Active effect (auto-switch follows what's on
                            // screen), named so glitch reports can say
                            // exactly which effect misbehaved.
                            effectName = GraniteCatalog.effectName(
                                Int(sparkamp_get_granite_effect(c)))
                        }
                    }
            }
            .allowsHitTesting(false)

            // Toast overlay
            let vars = themeManager.currentVars
            if toastVisible {
                VStack {
                    Spacer()
                    Text(toastMessage)
                        .font(vars.resolvedFont(16).weight(.semibold))
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
        }
        // WindowAccessor fires synchronously via viewDidMoveToWindow, before
        // the first layout pass.  We hide the window (alphaValue = 0) so the
        // initial 800×600 frame never flashes, then restore full opacity once
        // the OS fullscreen animation completes.
        .background(
            WindowAccessor { win in
                guard hostWindow == nil else { return }
                hostWindow = win
                win.alphaValue = 0
                win.toggleFullScreen(nil)
                NotificationCenter.default.addObserver(
                    forName: NSWindow.didEnterFullScreenNotification,
                    object: win,
                    queue: .main
                ) { _ in win.alphaValue = 1 }
            }
        )
        .onDisappear {
            model.fullscreenVizVisible = false
        }
        // No key handlers here: every shortcut (Esc, transport keys, `g` for
        // the FPS overlay, `j` exit-then-jump) is handled by the app-wide
        // key monitor in SparkampModel.handleRawKey. SwiftUI `.onKeyPress`
        // never fires for keys the monitor consumes, and focus on this view
        // is unreliable, so routing everything through the monitor is the
        // only dependable path.
        .focusable()
        // Needed so the window accepts key events at all, but never draw
        // the blue focus ring over the visualizer.
        .focusEffectDisabled()
        // Show a toast whenever the now-playing track (re)starts: next/prev,
        // play after pause/stop, or auto-advance. Driven by the model's
        // nonce (not currentTitle) so a same-track replay still toasts.
        // Uses the same "Artist — Title" convention as the marquee and
        // playlist rows (album-artist fallback; the raw title is already the
        // filename stem when the file has no tags).
        .onChange(of: model.nowPlayingNonce) { _, _ in
            let display = model.playlistItems
                .first(where: { $0.id == model.currentIndex })?
                .displayName ?? model.currentTitle
            guard !display.isEmpty else { return }
            showToast(display)
        }
    }

    private func showToast(_ message: String) {
        toastMessage = message
        withAnimation { toastVisible = true }
        // Restart the dismiss timer: cancel any pending hide first, so rapid
        // toasts (e.g. holding next/prev) keep the toast up a fresh 3 s each
        // time instead of an earlier hide firing partway through.
        toastDismiss?.cancel()
        let work = DispatchWorkItem { withAnimation { toastVisible = false } }
        toastDismiss = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 3, execute: work)
    }

}
