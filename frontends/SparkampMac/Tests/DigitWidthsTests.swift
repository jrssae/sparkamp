import XCTest
import AppKit

/// The player's LCD digit width, and the crash of 2026-10-16: three seconds
/// after a wake from sleep the font system returned no font, the nil reached
/// an attributes dictionary, and the app died. Nothing here can make macOS
/// do that on demand, so these hand `DigitWidths` a font lookup that fails.
@MainActor
final class DigitWidthsTests: XCTestCase {

    func testNoFontFallsBackToSFMonoProportionInsteadOfCrashing() {
        let widths = DigitWidths(font: { _ in nil })
        XCTAssertEqual(widths.width(at: 30), 30 * DigitWidths.fallbackEm, accuracy: 0.0001)
    }

    func testAFallbackIsNotRememberedSoTheNextRedrawMeasures() {
        var fontSystemUp = false
        var lookups = 0
        let widths = DigitWidths(font: { size in
            lookups += 1
            return fontSystemUp ? DigitWidths.systemMonospaced(size) : nil
        })

        XCTAssertEqual(widths.width(at: 30), 30 * DigitWidths.fallbackEm, accuracy: 0.0001)
        fontSystemUp = true
        let measured = widths.width(at: 30)
        XCTAssertEqual(lookups, 2, "the fallback must not be cached")

        XCTAssertEqual(widths.width(at: 30), measured)
        XCTAssertEqual(lookups, 2, "a real measurement is cached")
    }

    func testEachSizeIsMeasuredOnce() {
        var lookups = 0
        let widths = DigitWidths(font: { size in
            lookups += 1
            return DigitWidths.systemMonospaced(size)
        })
        _ = widths.width(at: 30)
        _ = widths.width(at: 24)
        _ = widths.width(at: 30)
        _ = widths.width(at: 24)
        XCTAssertEqual(lookups, 2)
    }

    /// The fallback is only honest if it matches what a measurement gives.
    func testFallbackProportionMatchesTheRealFont() {
        let measured = DigitWidths().width(at: 30)
        XCTAssertEqual(measured, 30 * DigitWidths.fallbackEm, accuracy: 0.01)
    }

    func testTheDescriptorRouteMeasuresTheSameAsTheSystemMonospacedFont() throws {
        let viaDescriptor = try XCTUnwrap(DigitWidths.systemMonospaced(30))
        let direct = NSFont.monospacedSystemFont(ofSize: 30, weight: .regular)
        let width = { (font: NSFont) in ("0" as NSString).size(withAttributes: [.font: font]).width }
        XCTAssertEqual(width(viaDescriptor), width(direct), accuracy: 0.0001)
    }

    /// A skin's font size comes out of a CSS file. Anything that is not a
    /// positive, finite number measures as the default size.
    func testUnusableSizesUseTheDefaultSize() {
        let widths = DigitWidths()
        let expected = widths.width(at: DigitWidths.defaultSize)
        for bad: CGFloat in [.nan, .infinity, -.infinity, 0, -12] {
            XCTAssertEqual(widths.width(at: bad), expected, "size \(bad)")
        }
    }

    func testWidthGrowsWithSize() {
        let widths = DigitWidths()
        XCTAssertLessThan(widths.width(at: 12), widths.width(at: 30))
    }
}
