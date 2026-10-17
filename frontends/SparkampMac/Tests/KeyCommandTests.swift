import XCTest

/// The keyboard map, and the shortcut list the user reads. A key that works
/// but is not listed, or is listed but does nothing, fails here.
@MainActor
final class KeyCommandTests: XCTestCase {

    private func resolve(
        _ chars: String?,
        keyCode: UInt16 = 0,
        modifiers: Bool = false,
        listFocused: Bool = false,
        fullscreen: Bool = false
    ) -> KeyCommand? {
        KeyCommand.resolve(chars: chars, keyCode: keyCode, hasModifiers: modifiers,
                           listFocused: listFocused, fullscreen: fullscreen)
    }

    /// What AppKit reports in `charactersIgnoringModifiers` for an arrow key.
    private func arrow(_ keyCode: UInt16, listFocused: Bool = false) -> KeyCommand? {
        resolve("\u{F700}", keyCode: keyCode, listFocused: listFocused)
    }

    func testTransportKeys() {
        XCTAssertEqual(resolve("z"), .previous)
        XCTAssertEqual(resolve("x"), .play)
        XCTAssertEqual(resolve("c"), .togglePlay)
        XCTAssertEqual(resolve("v"), .stop)
        XCTAssertEqual(resolve("V"), .stopWithFadeout)
        XCTAssertEqual(resolve("b"), .next)
        XCTAssertEqual(resolve("t"), .toggleStopAfterCurrent)
    }

    func testModeAndVolumeKeys() {
        XCTAssertEqual(resolve("r"), .cycleRepeat)
        XCTAssertEqual(resolve("s"), .toggleShuffle)
        XCTAssertEqual(resolve("-"), .volumeDown)
        XCTAssertEqual(resolve("="), .volumeUp)
        XCTAssertEqual(resolve("h"), .toggleRemainingTime)
    }

    func testKeysWithCommandOptionOrControlAreNeverSparkamps() {
        for key in ["z", "x", "b", "q", "n"] {
            XCTAssertNil(resolve(key, modifiers: true), key)
        }
        XCTAssertNil(resolve("\u{F700}", keyCode: KeyCommand.leftArrow, modifiers: true))
    }

    func testNoCharactersMeansNoCommand() {
        XCTAssertNil(resolve(nil, keyCode: KeyCommand.leftArrow))
    }

    func testUnmappedKeysReachWhateverHasFocus() {
        for key in ["y", "o", "Z", "B", "1", " ", "/"] {
            XCTAssertNil(resolve(key), key)
        }
    }

    func testArrowsSeekAndAdjustVolume() {
        XCTAssertEqual(arrow(KeyCommand.leftArrow), .seekBack)
        XCTAssertEqual(arrow(KeyCommand.rightArrow), .seekForward)
        XCTAssertEqual(arrow(KeyCommand.upArrow), .volumeUp)
        XCTAssertEqual(arrow(KeyCommand.downArrow), .volumeDown)
    }

    /// A focused track list keeps ↑/↓ for browsing rows; ←/→ still seek.
    func testAFocusedListKeepsUpAndDownButNotLeftAndRight() {
        XCTAssertNil(arrow(KeyCommand.upArrow, listFocused: true))
        XCTAssertNil(arrow(KeyCommand.downArrow, listFocused: true))
        XCTAssertEqual(arrow(KeyCommand.leftArrow, listFocused: true), .seekBack)
        XCTAssertEqual(arrow(KeyCommand.rightArrow, listFocused: true), .seekForward)
    }

    /// Opening a window over the fullscreen visualizer pulls macOS out of its
    /// Space, so those keys are swallowed there, and only there.
    func testWindowOpeningKeysAreSwallowedInFullscreenOnly() {
        for key in KeyCommand.blockedInFullscreenKeys {
            XCTAssertEqual(resolve(key, fullscreen: true), .blockedInFullscreen, key)
            let normal = resolve(key, fullscreen: false)
            XCTAssertNotNil(normal, key)
            XCTAssertNotEqual(normal, .blockedInFullscreen, key)
        }
    }

    func testJumpStillWorksInFullscreenBecauseItLeavesFullscreenFirst() {
        XCTAssertEqual(resolve("j", fullscreen: true), .openJump)
        XCTAssertEqual(resolve("j"), .openJump)
    }

    func testFullscreenOnlyKeys() {
        XCTAssertNil(resolve("g"))
        XCTAssertEqual(resolve("g", fullscreen: true), .toggleFpsOverlay)
        XCTAssertNil(resolve("\u{1B}"))
        XCTAssertEqual(resolve("\u{1B}", fullscreen: true), .exitFullscreen)
    }

    // MARK: The shortcut list

    /// Single keys listed in the Keyboard Shortcuts window, as the characters
    /// AppKit reports, and the arrow key codes listed there. Entries that are
    /// menu shortcuts (⌘O, ⌃Q), mouse gestures or table keys (Delete,
    /// Return) belong to menus and lists, not this key map, and are skipped.
    private static func listedKeys() -> (characters: Set<String>, arrows: Set<UInt16>) {
        let arrowCodes: [String: UInt16] = [
            "←": KeyCommand.leftArrow, "→": KeyCommand.rightArrow,
            "↑": KeyCommand.upArrow, "↓": KeyCommand.downArrow,
        ]
        var characters: Set<String> = []
        var arrows: Set<UInt16> = []
        for section in KeyboardShortcutsView.sections {
            for entry in section.entries {
                for part in entry.key.components(separatedBy: " / ") {
                    let token = part.trimmingCharacters(in: .whitespaces)
                    if let code = arrowCodes[token] {
                        arrows.insert(code)
                    } else if token == "Esc" {
                        characters.insert("\u{1B}")
                    } else if token == "−" {
                        characters.insert("-")   // the list prints a minus sign
                    } else if token.count == 2, token.first == "⇧", let letter = token.last {
                        characters.insert(String(letter).uppercased())
                    } else if token.count == 1 {
                        characters.insert(token)
                    }
                }
            }
        }
        return (characters, arrows)
    }

    func testEveryListedKeyDoesSomething() {
        let listed = Self.listedKeys()
        XCTAssertFalse(listed.characters.isEmpty)
        for key in listed.characters {
            // `g` and Esc are listed as fullscreen-only, so ask there too.
            let anywhere = resolve(key) ?? resolve(key, fullscreen: true)
            XCTAssertNotNil(anywhere, "listed but does nothing: \(key.debugDescription)")
        }
        for code in listed.arrows {
            XCTAssertNotNil(arrow(code), "listed arrow does nothing: \(code)")
        }
    }

    func testEveryKeyThatDoesSomethingIsListed() {
        let listed = Self.listedKeys()
        var keys = (0x21...0x7E).compactMap { UnicodeScalar($0).map { String(Character($0)) } }
        keys.append("\u{1B}")
        for key in keys {
            let normal = resolve(key)
            let fullscreen = resolve(key, fullscreen: true)
            let does = normal != nil || (fullscreen != nil && fullscreen != .blockedInFullscreen)
            if does {
                XCTAssertTrue(listed.characters.contains(key), "works but is not listed: \(key.debugDescription)")
            }
        }
        for code in [KeyCommand.leftArrow, KeyCommand.rightArrow, KeyCommand.upArrow, KeyCommand.downArrow] {
            XCTAssertTrue(listed.arrows.contains(code), "arrow \(code) works but is not listed")
        }
    }
}
