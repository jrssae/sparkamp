import XCTest

/// The "Send to" menu's shape: zero of a kind shows nothing, one shows a
/// direct item, two or more a submenu. Mirrors GTK's `send_to_spec`.
@MainActor
final class SendToSpecTests: XCTestCase {

    private let drive = SendTarget(id: "drive-1", label: "Slimtype DVD")
    private let drive2 = SendTarget(id: "drive-2", label: "MATSHITA DVD")
    private let stick = SendTarget(id: "dev-1", label: "SPEEDY")
    private let stick2 = SendTarget(id: "dev-2", label: "LINDY TECH")

    /// The spec as text, so a test can compare the whole menu at once.
    private func describe(_ entries: [SendEntry]) -> [String] {
        entries.map { entry in
            switch entry {
            case .activePlaylist: return "active"
            case .savedPlaylist: return "saved"
            case .driveDirect(let t): return "drive:\(t.id)"
            case .driveMenu(let ts): return "drives:" + ts.map(\.id).joined(separator: ",")
            case .deviceDirect(let t): return "device:\(t.id)"
            case .deviceMenu(let ts): return "devices:" + ts.map(\.id).joined(separator: ",")
            }
        }
    }

    func testNothingAttachedOffersOnlyThePlaylists() {
        XCTAssertEqual(describe(sendToSpec(drives: [], devices: [])), ["active", "saved"])
    }

    func testOneOfAKindIsADirectItem() {
        XCTAssertEqual(describe(sendToSpec(drives: [drive], devices: [stick])),
                       ["active", "saved", "drive:drive-1", "device:dev-1"])
    }

    func testTwoOrMoreAreASubmenuInTheOrderGiven() {
        XCTAssertEqual(describe(sendToSpec(drives: [drive2, drive], devices: [stick, stick2])),
                       ["active", "saved", "drives:drive-2,drive-1", "devices:dev-1,dev-2"])
    }

    func testKindsAreIndependent() {
        XCTAssertEqual(describe(sendToSpec(drives: [], devices: [stick])),
                       ["active", "saved", "device:dev-1"])
        XCTAssertEqual(describe(sendToSpec(drives: [drive, drive2], devices: [])),
                       ["active", "saved", "drives:drive-1,drive-2"])
    }
}
