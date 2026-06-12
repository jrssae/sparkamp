import SwiftUI
import AppKit

// MARK: - AppKit NSTableView helpers (shared by ActivePlaylistTable + MLEditorTable)

/// NSTableRowView subclass that paints selection with the active skin's
/// highlight colour at 18% alpha.  Returned by each Sparkamp NSTableView
/// wrapper from `tableView(_:rowViewForRow:)`.  Subclassing the row view
/// is more robust than the global `method_exchangeImplementations` swap
/// (which doesn't fire on every NSTableView style — `.inset` in
/// particular uses a CALayer-based selection path that bypasses
/// `drawSelection(in:)`).  Using a subclass guarantees AppKit calls our
/// override regardless of internal rendering changes.
///
/// The colour comes from `SparkampSelectionPalette.rowHighlight`, which
/// the ThemeManager keeps in sync with the active skin on launch and on
/// every skin switch.  Alpha is applied at draw time so non-system
/// colours (which can lose alpha through the SwiftUI Color → NSColor
/// bridge) still render at the correct opacity.
final class SparkampSkinRowView: NSTableRowView {
    /// AppKit invokes EITHER `drawBackground` + `drawSelection` (classic
    /// styles) OR a layered selection path (`.inset` etc.).  Overriding
    /// both covers every code path: drawSelection is no-op'd so AppKit's
    /// default bright-blue paint never runs; drawBackground does the
    /// actual skin-tinted fill on top of the row's normal background.
    override func drawSelection(in dirtyRect: NSRect) {
        // No-op on purpose — selection is drawn in `drawBackground`
        // below so the appearance is consistent across NSTableView
        // styles, including `.inset` which bypasses `drawSelection`.
    }

    override func drawBackground(in dirtyRect: NSRect) {
        super.drawBackground(in: dirtyRect)
        guard self.isSelected else { return }
        SparkampSelectionPalette.rowHighlight
            .withAlphaComponent(SparkampSelectionPalette.rowHighlightAlpha)
            .setFill()
        dirtyRect.fill()
    }
}

/// NSTableView subclass that forwards Delete / fn+Delete / Return / Enter
/// to SwiftUI-supplied callbacks, and lets the SwiftUI wrapper build a
/// context menu lazily on right-click.  Used by Sparkamp's NSViewRepresentable
/// wrappers so we get AppKit-native drag/drop click-vs-drag arbitration
/// (no SwiftUI .onDrag click-lag) while still routing actions through the
/// model layer in SwiftUI-style closures.
final class SparkampTableView: NSTableView {
    var onDeleteKey:   (() -> Void)?
    var onReturnKey:   (() -> Void)?
    var onContextMenu: ((NSEvent) -> NSMenu?)?

    override func keyDown(with event: NSEvent) {
        switch event.keyCode {
        case 51, 117:          // Delete (backspace) / fn+Delete (forward delete)
            onDeleteKey?()
        case 36, 76:           // Return / numpad Enter
            onReturnKey?()
        default:
            super.keyDown(with: event)
        }
    }

    override func menu(for event: NSEvent) -> NSMenu? {
        onContextMenu?(event) ?? super.menu(for: event)
    }
}

/// `NSMenuItem` subclass that fires an arbitrary closure on activation.
/// Avoids the @objc selector dance for every context-menu entry — the
/// closure captures whatever model/state the action needs.
final class BlockMenuItem: NSMenuItem {
    private var actionBlock: (() -> Void)?

    init(title: String, enabled: Bool = true, action: @escaping () -> Void) {
        super.init(title: title, action: #selector(fire), keyEquivalent: "")
        self.actionBlock = action
        self.target = self
        self.isEnabled = enabled
    }

    required init(coder: NSCoder) { fatalError("not implemented") }

    @objc private func fire() { actionBlock?() }
}

/// Reusable NSTableCellView that hosts a SwiftUI view via `NSHostingView`.
/// Cell reuse swaps the `rootView`, so AppKit's table-view recycling still
/// works.  Wrapping the SwiftUI content in `AnyView` lets one cell class
/// host different row types (PlaylistRow / ML editor row).
final class SparkampHostingCellView: NSTableCellView {
    private var hosting: NSHostingView<AnyView>?

    func setContent(_ view: AnyView) {
        if let h = hosting {
            h.rootView = view
        } else {
            let h = NSHostingView(rootView: view)
            h.translatesAutoresizingMaskIntoConstraints = false
            self.addSubview(h)
            NSLayoutConstraint.activate([
                h.leadingAnchor.constraint(equalTo: leadingAnchor),
                h.trailingAnchor.constraint(equalTo: trailingAnchor),
                h.topAnchor.constraint(equalTo: topAnchor),
                h.bottomAnchor.constraint(equalTo: bottomAnchor),
            ])
            hosting = h
        }
    }
}
