import SwiftUI
import AppKit
import ObjectiveC.runtime

// MARK: - App delegate
// Handles macOS-specific lifecycle events that SwiftUI's App protocol
// doesn't expose: dock-icon click, application reopen, terminate.

final class AppDelegate: NSObject, NSApplicationDelegate {

    /// Posted when the user clicks the dock icon or selects "Show Player"
    /// while no player window is visible.  The App scene listens for this
    /// and calls openWindow(id: "player").
    static let reopenPlayerNotification = Notification.Name("SparkampReopenPlayer")

    /// Called when the user clicks the dock icon with no visible windows,
    /// or chooses "Show All" / "SparkampMac" from the dock menu.
    func applicationShouldHandleReopen(_ sender: NSApplication,
                                       hasVisibleWindows flag: Bool) -> Bool {
        if !flag {
            NotificationCenter.default.post(name: Self.reopenPlayerNotification, object: nil)
        }
        return true
    }

    /// Keep running when the last window is closed (playlist or player).
    /// Matches Linux behaviour: audio keeps playing; use ⌘Q to quit.
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        false
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        // Swizzle NSTableRowView.drawSelection so every SwiftUI List / Table
        // paints its selection bar with the active skin's highlight colour
        // across the full row width (no per-cell gutter gaps).  The colour
        // is published to SparkampSelectionPalette by the ThemeManager.
        NSTableRowView.sparkampInstallSelectionPaint()

        // When any Sparkamp window is clicked, raise all other Sparkamp windows
        // so the complete set stays together in the window stack.
        NotificationCenter.default.addObserver(
            self,
            selector: #selector(windowDidBecomeKey(_:)),
            name: NSWindow.didBecomeKeyNotification,
            object: nil
        )
    }

    @objc private func windowDidBecomeKey(_ notification: Notification) {
        guard let keyWindow = notification.object as? NSWindow else { return }
        // Raise all visible, non-panel, non-sheet Sparkamp windows beneath the key window.
        let others = NSApp.windows.filter {
            $0 !== keyWindow &&
            $0.isVisible &&
            !$0.isMiniaturized &&
            !($0 is NSPanel) &&
            $0.sheetParent == nil
        }
        others.forEach { $0.orderFront(nil) }
        // Re-raise the key window on top of the group.
        keyWindow.orderFront(nil)
    }
}

// MARK: - Main app

@main
struct SparkampMacApp: App {

    @NSApplicationDelegateAdaptor(AppDelegate.self) var appDelegate
    @StateObject private var model        = SparkampModel()
    @StateObject private var themeManager = ThemeManager()

    var body: some Scene {
        // ── Main player ──────────────────────────────────────────────────────
        WindowGroup("Sparkamp", id: "player") {
            ContentView()
                .environmentObject(model)
                .environmentObject(themeManager)
                .themedRoot(themeManager)
                // Re-open this window when the dock icon is clicked while it
                // is hidden / closed.
                .onReceive(NotificationCenter.default.publisher(
                    for: AppDelegate.reopenPlayerNotification)) { _ in
                    // Bringing any key window to front is handled by the OS;
                    // this covers the case where the window was fully closed.
                    NSApp.windows.first { $0.title == "Sparkamp" }?.makeKeyAndOrderFront(nil)
                }
        }
        .windowStyle(.hiddenTitleBar)
        .windowResizability(.contentSize)
        .commands {
            SparkampCommands(model: model, themeManager: themeManager)
        }

        // ── Playlist (independent floating window) ───────────────────────────
        // model.playlistVisible == false at cold start, so this window is NOT
        // opened automatically; PlayerWindow opens it via openWindow(id:).
        Window("Playlist", id: "playlist") {
            PlaylistView()
                .environmentObject(model)
                .environmentObject(themeManager)
                .themedRoot(themeManager)
                .frame(minWidth: 360, idealWidth: 480, minHeight: 200, idealHeight: 400)
        }
        .windowResizability(.contentMinSize)
        .defaultSize(width: 480, height: 360)

        // ── Keyboard shortcuts (small fixed reference window) ─────────────────
        Window("Keyboard Shortcuts", id: "shortcuts") {
            KeyboardShortcutsView()
                .environmentObject(model)
                .environmentObject(themeManager)
                .themedRoot(themeManager)
        }
        .windowResizability(.contentSize)
        .defaultSize(width: 340, height: 420)

        // ── Fullscreen visualizer ─────────────────────────────────────────────
        // Opened programmatically from PlayerWindow when model.fullscreenVizVisible
        // becomes true.  The view itself calls toggleFullScreen via WindowAccessor.
        //
        // Intentionally a WindowGroup, not a unique Window: the fullscreen entry
        // depends on WindowAccessor.viewDidMoveToWindow firing on each open (it
        // grabs the fresh NSWindow and calls toggleFullScreen). A unique Window
        // reuses its instance, so on reopen that hook doesn't re-fire and the
        // window never enters fullscreen — which also blackens Granite, because
        // the mini view yields to fullscreen (single-driver rule) while the
        // fullscreen view never becomes visible enough to render. This window is
        // transient (always dismissed on Esc), so the duplicate-instance problem
        // that made the other windows unique doesn't apply here.
        WindowGroup("Visualizer", id: "fullscreen-viz") {
            FullscreenVisualizerView()
                .environmentObject(model)
                .environmentObject(themeManager)
                .themedRoot(themeManager)
        }
        .windowStyle(.hiddenTitleBar)
        .windowResizability(.contentMinSize)
        .defaultSize(width: 800, height: 600)

        // ── Jump to Track ─────────────────────────────────────────────────────
        Window("Jump to Track", id: "jump-to-track") {
            JumpToTrackView()
                .environmentObject(model)
                .environmentObject(themeManager)
                .themedRoot(themeManager)
        }
        .windowResizability(.contentMinSize)
        .defaultSize(width: 480, height: 360)

        // ── Play Queue ──────────────────────────────────────────────────────────
        Window("Play Queue", id: "queue") {
            QueueView()
                .environmentObject(model)
                .environmentObject(themeManager)
                .themedRoot(themeManager)
        }
        .windowResizability(.contentMinSize)
        .defaultSize(width: 420, height: 420)

        // ── Equalizer ─────────────────────────────────────────────────────────
        Window("Equalizer", id: "equalizer") {
            EqualizerView()
                .environmentObject(model)
                .environmentObject(themeManager)
                .themedRoot(themeManager)
        }
        .windowResizability(.contentSize)
        .defaultSize(width: 480, height: 320)

        // ── Settings ──────────────────────────────────────────────────────────
        Window("Settings", id: "settings") {
            SettingsView()
                .environmentObject(model)
                .environmentObject(themeManager)
                .themedRoot(themeManager)
        }
        .windowResizability(.contentMinSize)
        .defaultSize(width: 480, height: 500)

        // ── ID3 Tag Editor ────────────────────────────────────────────────────
        Window("Tag Editor", id: "id3-editor") {
            Id3EditorView()
                .environmentObject(model)
                .environmentObject(themeManager)
                .themedRoot(themeManager)
        }
        .windowResizability(.contentMinSize)
        .defaultSize(width: 560, height: 500)

        // ── Artwork zoom window ───────────────────────────────────────────────
        Window("Artwork", id: "artwork") {
            ArtworkView()
                .environmentObject(model)
                .environmentObject(themeManager)
                .themedRoot(themeManager)
        }
        .windowResizability(.contentSize)
        .defaultSize(width: 512, height: 512)

        // ── Media Library ─────────────────────────────────────────────────────
        Window("Media Library", id: "media-library") {
            MediaLibraryView()
                .environmentObject(model)
                .environmentObject(themeManager)
                .themedRoot(themeManager)
        }
        .windowResizability(.contentMinSize)
        .defaultSize(width: 800, height: 520)

        // ── Deduplicator ──────────────────────────────────────────────────────
        Window("Find Duplicates", id: "deduplicator") {
            DeduplicatorView()
                .environmentObject(model)
                .environmentObject(themeManager)
                .themedRoot(themeManager)
        }
        .windowResizability(.contentMinSize)
        .defaultSize(width: 600, height: 480)
    }
}

// MARK: - Menu commands

struct SparkampCommands: Commands {
    let model: SparkampModel
    let themeManager: ThemeManager

    var body: some Commands {
        CommandGroup(replacing: .newItem) {
            Button("Add File…") { model.openFilePicker() }
                .keyboardShortcut("o", modifiers: .command)
            Button("Clear Playlist") { model.clearPlaylist() }
        }

        CommandMenu("Playback") {
            Button("Play / Pause")   { model.togglePlay() }
                .keyboardShortcut("c", modifiers: [])
            Button("Stop")           { model.stop() }
                .keyboardShortcut("v", modifiers: [])
            Button("Previous")       { model.prev() }
                .keyboardShortcut("z", modifiers: [])
            Button("Next")           { model.next() }
                .keyboardShortcut("b", modifiers: [])
            Divider()
            Button("Cycle Repeat")   { model.cycleRepeat() }
                .keyboardShortcut("r", modifiers: [])
            Button("Toggle Shuffle") { model.toggleShuffle() }
                .keyboardShortcut("s", modifiers: [])
            Divider()
            Button("Cycle Visualizer Mode") { model.cycleVizMode() }
                .keyboardShortcut("a", modifiers: [])
            Button("Fullscreen Visualizer") { model.openFullscreenViz() }
                .keyboardShortcut("f", modifiers: [])
            Button("Jump to Track…") { model.jumpToTrackVisible.toggle() }
                .keyboardShortcut("j", modifiers: [])
            Button("Equalizer…")     { model.equalizerVisible.toggle() }
                .keyboardShortcut("u", modifiers: [])
            Button("Edit Tags…")     { model.openId3Editor() }
                .keyboardShortcut("d", modifiers: [])
        }

        CommandMenu("Appearance") {
            Button("Dark Theme")  { themeManager.setActiveSkin("dark") }
            Button("Light Theme") { themeManager.setActiveSkin("light") }
            Divider()
            Button("Skin Settings…") { model.settingsVisible = true }
        }

        // Replace the default Window menu so "Show Player" appears alongside
        // "Show Playlist".  Both are always reachable from the menu bar even
        // when the windows are closed.
        CommandGroup(replacing: .windowList) {
            Button("Show Player") {
                // Bring the player window forward; if it was closed the dock
                // reopen path will recreate it on next activation.
                NSApp.windows.first { $0.title == "Sparkamp" }?.makeKeyAndOrderFront(nil)
                NotificationCenter.default.post(
                    name: AppDelegate.reopenPlayerNotification, object: nil)
            }
            .keyboardShortcut("0", modifiers: .command)

            Button("Show Playlist") { model.playlistVisible = true }
                .keyboardShortcut("p", modifiers: [])

            Button("Equalizer") { model.equalizerVisible.toggle() }
            Button("Settings")  { model.settingsVisible.toggle() }
            Button("Media Library") { model.openMediaLibrary() }
                .keyboardShortcut("l", modifiers: .command)
            Button("Find Duplicates") { model.dedupVisible = true }

            Button("Keyboard Shortcuts") { model.keyboardShortcutsVisible.toggle() }
                .keyboardShortcut("i", modifiers: [])
        }
    }
}

// MARK: - Notification names

extension Notification.Name {
    static let openFilePicker = Notification.Name("SparkampOpenFilePicker")
}

// MARK: - NSTableRowView skin-tinted selection paint
//
// AppKit lacks UIKit-style `appearance()` proxies, so we swizzle
// `NSTableRowView.drawSelection(in:)` to paint the active skin's highlight
// across the full row.  This solves two problems:
//
// 1. The default AppKit selection uses `NSColor.selectedContentBackgroundColor`
//    (system accent) which ignores SwiftUI `.tint()` — selection would not
//    match the skin.
// 2. Per-cell `.background()` painting in SwiftUI `Table` leaves visible
//    gaps between columns where SwiftUI doesn't extend the cell view into
//    the inter-column gutter.  Painting at the row level via NSTableRowView
//    fills the whole row continuously, matching the look of List's
//    `.listRowBackground`.
//
// The colour comes from `SparkampSelectionPalette.rowHighlight`, which the
// ThemeManager keeps in sync with the active skin on launch + every switch.

/// Static colour bridge between the SwiftUI ThemeManager and the AppKit
/// swizzle.  AppKit calls `drawSelection(in:)` on its own thread cycle and
/// has no access to SwiftUI environment values, so we publish the active
/// skin's row-highlight colour through this main-thread-only holder.
enum SparkampSelectionPalette {
    /// Skin highlight used to paint full-row selection in every NSTableView.
    /// Default to the system accent until the ThemeManager init runs.  This
    /// is the FULL-opacity skin highlight; the swizzled `drawSelection`
    /// applies the row-selection alpha at draw time so the published value
    /// matches the skin's CSS `highlight` exactly (NSColor↔SwiftUI Color
    /// bridging can drop alpha for non-system colours, so opacity is
    /// applied with `withAlphaComponent` at the draw site instead).
    static var rowHighlight: NSColor = .controlAccentColor

    /// Alpha used to tint the full-row selection paint.  Matches the
    /// `playlistSelectedBg` formula in `SkinTheme` (highlight × 0.18).
    static let rowHighlightAlpha: CGFloat = 0.18
}

extension NSTableRowView {
    private static let installOnce: Void = {
        let cls = NSTableRowView.self
        guard
            let original = class_getInstanceMethod(
                cls, #selector(NSTableRowView.drawSelection(in:))),
            let replacement = class_getInstanceMethod(
                cls, #selector(NSTableRowView.sparkamp_drawSkinSelection(in:)))
        else { return }
        method_exchangeImplementations(original, replacement)
    }()

    /// Install the skin-tinted selection paint hook.  Safe to call multiple
    /// times; the swizzle runs once.
    static func sparkampInstallSelectionPaint() { _ = installOnce }

    /// Skin-tinted full-row selection paint.  Runs in place of the original
    /// `drawSelection(in:)` after the swizzle is installed.  Only paints
    /// when the row is actually selected so non-selected rows stay
    /// transparent and let `.listRowBackground` show through.
    @objc func sparkamp_drawSkinSelection(in dirtyRect: NSRect) {
        guard self.isSelected else { return }
        SparkampSelectionPalette.rowHighlight
            .withAlphaComponent(SparkampSelectionPalette.rowHighlightAlpha)
            .setFill()
        self.bounds.fill()
    }
}

// MARK: - Themed root modifier

/// Applies the four root-level theming defaults (font, foreground, tint,
/// preferred color scheme) so every SwiftUI view inside a WindowGroup
/// inherits the active skin without repeating the modifiers per-view.
///
/// Observes `ThemeManager` so re-evaluation happens when the user switches
/// skins.
private struct ThemedRootModifier: ViewModifier {
    @ObservedObject var themeManager: ThemeManager

    func body(content: Content) -> some View {
        let v = themeManager.currentVars
        content
            .font(v.bodyFont)
            .foregroundStyle(v.textColor)
            .tint(v.highlight)
            .preferredColorScheme(v.prefersDark ? .dark : .light)
    }
}

private extension View {
    /// Apply the active skin's body font, text color, accent tint, and
    /// preferred color scheme to a WindowGroup root view.
    func themedRoot(_ themeManager: ThemeManager) -> some View {
        modifier(ThemedRootModifier(themeManager: themeManager))
    }
}
