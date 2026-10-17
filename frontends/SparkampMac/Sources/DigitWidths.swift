import AppKit

/// Advance widths of one SF Mono digit, by font size, for the player's LCD
/// column.
///
/// Measured once per size rather than on every redraw: the player's body
/// redraws ten times a second, and each redraw used to ask the font system
/// again.
///
/// Every step can come back empty, and each is checked. The obvious call,
/// `NSFont.monospacedSystemFont`, is declared never to return nil, and did
/// anyway three seconds after a wake from sleep (crash of 2026-10-16): the nil
/// went into an attributes dictionary, whose initializer threw, and that
/// killed the app. The descriptor route below gives the same font
/// (`.SFNSMono-Regular`, measured identical widths) through optional APIs.
@MainActor
final class DigitWidths {
    static let shared = DigitWidths()

    /// SF Mono's digit advance as a fraction of the point size: 1266/2048.
    /// What a measurement gives when the font system answers.
    static let fallbackEm: CGFloat = 0.618

    /// The size used when a skin's is not a usable number.
    static let defaultSize: CGFloat = 30

    private let font: (CGFloat) -> NSFont?
    private var widths: [CGFloat: CGFloat] = [:]

    /// `font` looks up the face to measure. The default asks the font system;
    /// tests pass their own to stand in for a font system that will not answer.
    init(font: @escaping (CGFloat) -> NSFont? = DigitWidths.systemMonospaced) {
        self.font = font
    }

    /// The system monospaced face, by descriptor, so a font system that will
    /// not answer gives nil rather than a nil that claims not to be one.
    nonisolated static func systemMonospaced(_ size: CGFloat) -> NSFont? {
        guard let descriptor = NSFontDescriptor.preferredFontDescriptor(forTextStyle: .body)
            .withDesign(.monospaced)
        else { return nil }
        return NSFont(descriptor: descriptor, size: size)
    }

    /// One digit's advance at `requested` points.
    ///
    /// A size that is not a positive, finite number is replaced by
    /// `defaultSize`. When the font system will not answer, this returns
    /// `fallbackEm` times the size and does not remember it, so the next call
    /// measures properly once the font system is back.
    func width(at requested: CGFloat) -> CGFloat {
        let size = requested.isFinite && requested > 0 ? requested : Self.defaultSize
        if let width = widths[size] { return width }
        let fallback = size * Self.fallbackEm
        guard let font = font(size) else { return fallback }
        let width = ("0" as NSString).size(withAttributes: [.font: font]).width
        guard width.isFinite, width > 0 else { return fallback }
        widths[size] = width
        return width
    }
}
