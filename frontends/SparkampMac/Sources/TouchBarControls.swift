import AppKit
import Combine

// MARK: - Touch Bar controls (mac-only extra)
//
// Shown in the app region of the Touch Bar while Sparkamp is the active app.
// Separate from the system Now Playing strip (SparkampModel+NowPlaying.swift):
// the system strip only ever offers play/pause behind the Control Strip's
// expand arrow, so prev/next/scrub/mode buttons need our own bar.
//
// AppKit rather than SwiftUI's `.touchBar` modifier: that modifier only
// activates when its view is in the *focused* responder chain, which a plain
// window-root container never becomes, so it silently produced no bar. The
// Touch Bar responder chain ends at NSApp and then the app delegate, so
// providing the bar from the delegate (NSTouchBarProvider) makes it apply
// app-wide with no focus requirements.
//
// Every control routes to the same model transport methods as the on-screen
// buttons and the keyboard shortcuts, so behavior can't drift between them.

extension NSTouchBarItem.Identifier {
    static let sparkampPrev      = NSTouchBarItem.Identifier("dev.sparkamp.touchbar.prev")
    static let sparkampPlayPause = NSTouchBarItem.Identifier("dev.sparkamp.touchbar.playPause")
    static let sparkampStop      = NSTouchBarItem.Identifier("dev.sparkamp.touchbar.stop")
    static let sparkampNext      = NSTouchBarItem.Identifier("dev.sparkamp.touchbar.next")
    static let sparkampSeek      = NSTouchBarItem.Identifier("dev.sparkamp.touchbar.seek")
    static let sparkampRepeat    = NSTouchBarItem.Identifier("dev.sparkamp.touchbar.repeat")
    static let sparkampShuffle   = NSTouchBarItem.Identifier("dev.sparkamp.touchbar.shuffle")
}

@MainActor
final class SparkampTouchBarController: NSObject, NSTouchBarDelegate {

    /// Set once the SwiftUI scene has built the model (see SparkampMacApp).
    weak var model: SparkampModel? {
        didSet { subscribe() }
    }

    private var cancellables = Set<AnyCancellable>()
    private var playPauseButton: NSButton?
    private var repeatButton:    NSButton?
    private var shuffleButton:   NSButton?
    private var seekItem:        NSSliderTouchBarItem?

    /// While the user's finger is on the scrubber we must not overwrite its
    /// value with the 10 Hz position stream, and we want ONE seek at the end of
    /// the drag rather than dozens mid-drag. `suppressUntil` blocks incoming
    /// updates briefly after each touch; `pendingSeek` debounces the seek.
    private var suppressUntil: Date?
    private var pendingSeek: DispatchWorkItem?

    // MARK: Bar construction

    /// The one bar instance, shared by every responder we install it on (see
    /// `install(on:)`). Built once so the button references captured while
    /// making items stay valid for live state updates.
    private(set) lazy var bar: NSTouchBar = makeTouchBar()

    /// Install the bar on a responder. AppKit walks first responder → window →
    /// NSApp → app delegate looking for one; which link actually gets consulted
    /// varies with what SwiftUI puts in the chain, so we set it on the window
    /// and NSApp directly rather than relying on the delegate alone.
    func install(on responder: NSResponder?) {
        responder?.touchBar = bar
    }

    func makeTouchBar() -> NSTouchBar {
        let bar = NSTouchBar()
        bar.delegate = self
        bar.customizationIdentifier = NSTouchBar.CustomizationIdentifier("dev.sparkamp.touchbar")
        bar.defaultItemIdentifiers = [
            .sparkampPrev, .sparkampPlayPause, .sparkampStop, .sparkampNext,
            .sparkampSeek,
            .sparkampRepeat, .sparkampShuffle,
        ]
        bar.customizationAllowedItemIdentifiers = bar.defaultItemIdentifiers
        return bar
    }

    func touchBar(_ touchBar: NSTouchBar,
                  makeItemForIdentifier identifier: NSTouchBarItem.Identifier) -> NSTouchBarItem? {
        switch identifier {
        case .sparkampPrev:
            return buttonItem(identifier, symbol: "backward.fill",
                              label: "Previous", action: #selector(tbPrev))
        case .sparkampPlayPause:
            let item = buttonItem(identifier, symbol: "play.fill",
                                  label: "Play/Pause", action: #selector(tbPlayPause))
            playPauseButton = item.view as? NSButton
            refreshTransportIcons()
            return item
        case .sparkampStop:
            return buttonItem(identifier, symbol: "stop.fill",
                              label: "Stop", action: #selector(tbStop))
        case .sparkampNext:
            return buttonItem(identifier, symbol: "forward.fill",
                              label: "Next", action: #selector(tbNext))
        case .sparkampRepeat:
            let item = buttonItem(identifier, symbol: "repeat",
                                  label: "Repeat", action: #selector(tbRepeat))
            repeatButton = item.view as? NSButton
            refreshModeIcons()
            return item
        case .sparkampShuffle:
            let item = buttonItem(identifier, symbol: "shuffle",
                                  label: "Shuffle", action: #selector(tbShuffle))
            shuffleButton = item.view as? NSButton
            refreshModeIcons()
            return item
        case .sparkampSeek:
            let item = NSSliderTouchBarItem(identifier: identifier)
            item.slider.minValue = 0
            item.slider.maxValue = 1
            item.slider.doubleValue = 0
            // Continuous so we can tell the user is dragging; the actual seek
            // is debounced to the end of the gesture.
            item.slider.isContinuous = true
            item.target = self
            item.action = #selector(tbSeek(_:))
            item.customizationLabel = "Seek"
            seekItem = item
            refreshSeek()
            return item
        default:
            return nil
        }
    }

    private func buttonItem(_ identifier: NSTouchBarItem.Identifier,
                            symbol: String,
                            label: String,
                            action: Selector) -> NSCustomTouchBarItem {
        let item = NSCustomTouchBarItem(identifier: identifier)
        let image = NSImage(systemSymbolName: symbol, accessibilityDescription: label)
        let button = NSButton(image: image ?? NSImage(), target: self, action: action)
        button.bezelStyle = .rounded
        item.view = button
        item.customizationLabel = label
        return item
    }

    // MARK: Actions

    @objc private func tbPrev()      { model?.prev() }
    @objc private func tbPlayPause() { model?.togglePlay() }
    @objc private func tbStop()      { model?.stop() }
    @objc private func tbNext()      { model?.next() }
    @objc private func tbRepeat()    { model?.cycleRepeat() }
    @objc private func tbShuffle()   { model?.toggleShuffle() }

    @objc private func tbSeek(_ sender: Any?) {
        guard let item = seekItem else { return }
        let fraction = item.slider.doubleValue
        // Hold off the position stream while the finger is down…
        suppressUntil = Date().addingTimeInterval(0.4)
        // …and seek once, shortly after the last movement.
        pendingSeek?.cancel()
        let work = DispatchWorkItem { [weak self] in
            self?.model?.seek(to: fraction)
        }
        pendingSeek = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.25, execute: work)
    }

    // MARK: Live state

    /// Mirror model state onto the bar. Subscriptions are per-model, so this is
    /// re-armed whenever `model` is assigned.
    private func subscribe() {
        cancellables.removeAll()
        guard let model else { return }
        // `@Published` fires its publisher in `willSet`, so a handler that
        // re-reads the model here would still see the OLD value and the bar
        // would render one state behind (tap repeat → UI shows One, bar still
        // shows Off). Hopping to the next main-queue turn lets the assignment
        // land first, so the refreshes below read current state.
        model.$isPlaying
            .receive(on: DispatchQueue.main)
            .sink { [weak self] _ in self?.refreshTransportIcons() }
            .store(in: &cancellables)
        model.$repeatMode
            .receive(on: DispatchQueue.main)
            .sink { [weak self] _ in self?.refreshModeIcons() }
            .store(in: &cancellables)
        model.$shuffleEnabled
            .receive(on: DispatchQueue.main)
            .sink { [weak self] _ in self?.refreshModeIcons() }
            .store(in: &cancellables)
        model.$position
            .receive(on: DispatchQueue.main)
            .sink { [weak self] _ in self?.refreshSeek() }
            .store(in: &cancellables)
    }

    private func refreshTransportIcons() {
        let playing = model?.isPlaying ?? false
        playPauseButton?.image = NSImage(
            systemSymbolName: playing ? "pause.fill" : "play.fill",
            accessibilityDescription: playing ? "Pause" : "Play")
    }

    private func refreshModeIcons() {
        guard let model else { return }
        // Repeat cycles Off → One → All; "repeat.1" marks single-track repeat,
        // and the button reads as pressed whenever repeat is on at all.
        repeatButton?.image = NSImage(
            systemSymbolName: model.repeatMode == 1 ? "repeat.1" : "repeat",
            accessibilityDescription: "Repeat")
        repeatButton?.state  = model.repeatMode == 0 ? .off : .on
        repeatButton?.bezelColor = model.repeatMode == 0 ? nil : .controlAccentColor
        shuffleButton?.state = model.shuffleEnabled ? .on : .off
        shuffleButton?.bezelColor = model.shuffleEnabled ? .controlAccentColor : nil
    }

    private func refreshSeek() {
        guard let item = seekItem, let model else { return }
        if let until = suppressUntil, Date() < until { return }
        let duration = model.duration
        item.slider.isEnabled = duration > 0
        item.slider.doubleValue = duration > 0
            ? min(1, max(0, model.position / duration))
            : 0
    }
}
