import XCTest
import SwiftUI
import AppKit

/// Skins: hex colours, the CSS parser, the values derived from a skin, and
/// the built-in skins against the Swift defaults that claim to mirror them.
@MainActor
final class ThemeTests: XCTestCase {

    private struct RGBA: Equatable {
        var r, g, b, a: Double

        static func of(_ color: Color) -> RGBA {
            let ns = NSColor(color).usingColorSpace(.sRGB) ?? .clear
            return RGBA(r: ns.redComponent, g: ns.greenComponent,
                        b: ns.blueComponent, a: ns.alphaComponent)
        }

        func isClose(to other: RGBA, within tolerance: Double = 0.002) -> Bool {
            abs(r - other.r) <= tolerance && abs(g - other.g) <= tolerance
                && abs(b - other.b) <= tolerance && abs(a - other.a) <= tolerance
        }
    }

    private func assertColor(_ hex: String, _ expected: RGBA,
                             file: StaticString = #filePath, line: UInt = #line) {
        guard let color = Color(hex: hex) else {
            return XCTFail("\(hex.debugDescription) did not parse", file: file, line: line)
        }
        let got = RGBA.of(color)
        XCTAssertTrue(got.isClose(to: expected), "\(hex): \(got) vs \(expected)", file: file, line: line)
    }

    // MARK: Hex colours

    func testShortLongAndAlphaHexForms() {
        assertColor("#fff", RGBA(r: 1, g: 1, b: 1, a: 1))
        assertColor("#000000", RGBA(r: 0, g: 0, b: 0, a: 1))
        assertColor("#ff8000", RGBA(r: 1, g: 128.0 / 255, b: 0, a: 1))
        assertColor("#11223344", RGBA(r: 17.0 / 255, g: 34.0 / 255, b: 51.0 / 255, a: 68.0 / 255))
    }

    func testHexToleratesCaseWhitespaceAndAMissingHash() {
        assertColor("#ABC", RGBA(r: 10.0 / 15, g: 11.0 / 15, b: 12.0 / 15, a: 1))
        assertColor("  #abc\n", RGBA(r: 10.0 / 15, g: 11.0 / 15, b: 12.0 / 15, a: 1))
        assertColor("abc", RGBA(r: 10.0 / 15, g: 11.0 / 15, b: 12.0 / 15, a: 1))
    }

    func testMalformedHexIsRejected() {
        for bad in ["", "#", "#12", "#12345", "#1234567", "#123456789",
                    "#ggg", "#12g456", "#12 456", "fff !important", "red", "#ＡＢＣ"] {
            XCTAssertNil(Color(hex: bad), "accepted \(bad.debugDescription)")
        }
    }

    // MARK: CSS parser

    private func assertVars(_ a: SkinVars, equal b: SkinVars,
                            file: StaticString = #filePath, line: UInt = #line) {
        let colors: [(String, Color, Color)] = [
            ("background", a.background, b.background),
            ("textBackground", a.textBackground, b.textBackground),
            ("textColor", a.textColor, b.textColor),
            ("highlight", a.highlight, b.highlight),
            ("brokenColor", a.brokenColor, b.brokenColor),
            ("buttonColor", a.buttonColor, b.buttonColor),
            ("buttonHover", a.buttonHover, b.buttonHover),
            ("buttonActive", a.buttonActive, b.buttonActive),
            ("buttonPressed", a.buttonPressed, b.buttonPressed),
            ("buttonTextColor", a.buttonTextColor, b.buttonTextColor),
        ]
        for (name, x, y) in colors {
            XCTAssertTrue(RGBA.of(x).isClose(to: RGBA.of(y)), name, file: file, line: line)
        }
        XCTAssertEqual(a.fontFamily, b.fontFamily, file: file, line: line)
        XCTAssertEqual(a.fontSize, b.fontSize, accuracy: 0.001, file: file, line: line)
        XCTAssertEqual(a.fontSizeLarge, b.fontSizeLarge, accuracy: 0.001, file: file, line: line)
        XCTAssertEqual(a.fontSizeMarquee, b.fontSizeMarquee, accuracy: 0.001, file: file, line: line)
    }

    func testEmptyOrRootlessCSSGivesTheDarkDefaults() {
        assertVars(CSSParser.parse(css: ""), equal: .dark)
        assertVars(CSSParser.parse(css: "body { --sp-background: #ffffff; }"), equal: .dark)
    }

    func testVariablesAreReadAndUnknownOnesIgnored() {
        let vars = CSSParser.parse(css: """
            :root {
              --sp-background: #ffffff;
              --sp-highlight: #ff0000;
              --sp-unknown-thing: 12px;
              not-a-variable: #000;
              --sp-text-color #00ff00;
            }
            """)
        XCTAssertTrue(RGBA.of(vars.background).isClose(to: RGBA(r: 1, g: 1, b: 1, a: 1)))
        XCTAssertTrue(RGBA.of(vars.highlight).isClose(to: RGBA(r: 1, g: 0, b: 0, a: 1)))
        // The statement with no colon is skipped; the default stays.
        XCTAssertTrue(RGBA.of(vars.textColor).isClose(to: RGBA.of(SkinVars.dark.textColor)))
    }

    /// Sizes are CSS px, shared verbatim with the Linux build; 1px is 0.75pt.
    func testPixelSizesBecomePoints() {
        let vars = CSSParser.parse(css: """
            :root { --sp-font-size: 15px; --sp-font-size-large: 40px; --sp-font-size-marquee: 20; }
            """)
        XCTAssertEqual(vars.fontSize, 11.25, accuracy: 0.001)
        XCTAssertEqual(vars.fontSizeLarge, 30, accuracy: 0.001)
        XCTAssertEqual(vars.fontSizeMarquee, 15, accuracy: 0.001, "a bare number counts as px")
    }

    /// `Double` parses all of these. None is a size AppKit can draw.
    func testUnusableSizesKeepTheDefault() {
        for bad in ["nan", "NaN", "inf", "-inf", "-12px", "0px", "0", "abc", "12pt", ""] {
            let vars = CSSParser.parse(css: ":root { --sp-font-size-large: \(bad); }")
            XCTAssertEqual(vars.fontSizeLarge, SkinVars.dark.fontSizeLarge, "accepted \(bad.debugDescription)")
        }
    }

    func testCommentsAreIgnoredEvenInsideTheBlock() {
        let vars = CSSParser.parse(css: """
            /* :root { --sp-background: #ff0000; } */
            :root {
              /* --sp-highlight: #ff0000; */
              --sp-background: #ffffff; /* trailing */
            }
            """)
        XCTAssertTrue(RGBA.of(vars.background).isClose(to: RGBA(r: 1, g: 1, b: 1, a: 1)))
        XCTAssertTrue(RGBA.of(vars.highlight).isClose(to: RGBA.of(SkinVars.dark.highlight)))
    }

    func testAnUnterminatedCommentEndsTheFile() {
        let vars = CSSParser.parse(css: ":root { --sp-background: #ffffff; /* --sp-highlight: #ff0000; }")
        // The block never closes, so nothing in it counts.
        assertVars(vars, equal: .dark)
    }

    func testTheLaterOfTwoDeclarationsWins() {
        let vars = CSSParser.parse(css: ":root { --sp-background: #000000; --sp-background: #ffffff; }")
        XCTAssertTrue(RGBA.of(vars.background).isClose(to: RGBA(r: 1, g: 1, b: 1, a: 1)))
    }

    func testFontFamilyQuotesAndTheFirstFamily() {
        let quoted = CSSParser.parse(css: ":root { --sp-font-family: \"Fira Code\"; }")
        XCTAssertEqual(quoted.fontFamily, "Fira Code")
        XCTAssertEqual(quoted.primaryFontFamily, "Fira Code")

        let list = CSSParser.parse(css: ":root { --sp-font-family: 'Fira Code', monospace; }")
        XCTAssertEqual(list.primaryFontFamily, "Fira Code")
        XCTAssertEqual(SkinVars.dark.primaryFontFamily, "Inter")
    }

    func testHasRootBlock() {
        XCTAssertTrue(CSSParser.hasRootBlock(":root { }"))
        XCTAssertTrue(CSSParser.hasRootBlock("/* skin */ :root{--sp-background:#fff}"))
        XCTAssertFalse(CSSParser.hasRootBlock("body { color: red; }"))
        XCTAssertFalse(CSSParser.hasRootBlock("/* :root { } */"))
        XCTAssertFalse(CSSParser.hasRootBlock(":root"))
    }

    // MARK: Built-in skins

    /// `SkinVars.dark` and `.light` say they mirror the core's defaults. The
    /// built-in skins' CSS comes from the core, so parsing it must give
    /// exactly the Swift values; a change on either side fails here.
    func testBuiltInDarkSkinMatchesTheSwiftDefaults() {
        XCTAssertTrue(CSSParser.hasRootBlock(ThemeManager.darkTemplateCSS))
        assertVars(CSSParser.parse(css: ThemeManager.darkTemplateCSS), equal: .dark)
    }

    func testBuiltInLightSkinMatchesTheSwiftDefaults() {
        XCTAssertTrue(CSSParser.hasRootBlock(ThemeManager.lightTemplateCSS))
        assertVars(CSSParser.parse(css: ThemeManager.lightTemplateCSS), equal: .light)
    }

    // MARK: Derived values

    func testDarkAndLightSkinsAreRecognised() {
        XCTAssertTrue(SkinVars.dark.prefersDark)
        XCTAssertFalse(SkinVars.light.prefersDark)
    }

    func testBorderIsLighterOnDarkSkinsAndDarkerOnLightOnes() {
        let darkBg = RGBA.of(SkinVars.dark.background)
        let darkBorder = RGBA.of(SkinVars.dark.borderColor)
        XCTAssertGreaterThan(darkBorder.r, darkBg.r)

        let lightBg = RGBA.of(SkinVars.light.background)
        let lightBorder = RGBA.of(SkinVars.light.borderColor)
        XCTAssertLessThan(lightBorder.r, lightBg.r)
    }

    func testBorderStaysInRangeOnPureBlackAndWhite() {
        var black = SkinVars.dark
        black.background = Color(hex: "#000000")!
        XCTAssertTrue(RGBA.of(black.borderColor).isClose(to: RGBA(r: 0.08, g: 0.08, b: 0.08, a: 1)))

        var white = SkinVars.light
        white.background = Color(hex: "#ffffff")!
        XCTAssertTrue(RGBA.of(white.borderColor).isClose(to: RGBA(r: 0.92, g: 0.92, b: 0.92, a: 1)))
    }
}
