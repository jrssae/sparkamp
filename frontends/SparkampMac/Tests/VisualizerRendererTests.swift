import XCTest
import SwiftUI

/// The visualizer's colour-zone arithmetic, which runs for every bar and
/// every waveform segment, every frame. A bad index here is a crash in the
/// middle of playback.
@MainActor
final class VisualizerRendererTests: XCTestCase {

    func testZonesRunFromTheBottomUp() {
        let h: CGFloat = 100
        XCTAssertEqual(VisualizerRenderer.zoneForY(100, height: h, numZones: 5), 0, "bottom edge")
        XCTAssertEqual(VisualizerRenderer.zoneForY(90, height: h, numZones: 5), 0)
        XCTAssertEqual(VisualizerRenderer.zoneForY(50, height: h, numZones: 5), 2)
        XCTAssertEqual(VisualizerRenderer.zoneForY(10, height: h, numZones: 5), 4)
        XCTAssertEqual(VisualizerRenderer.zoneForY(0, height: h, numZones: 5), 4, "top edge stays in range")
    }

    func testPointsOutsideTheViewStayInRange() {
        XCTAssertEqual(VisualizerRenderer.zoneForY(-30, height: 100, numZones: 5), 4)
        XCTAssertEqual(VisualizerRenderer.zoneForY(250, height: 100, numZones: 5), 0)
    }

    /// A zero-height view divided zero by zero, and `Int(.nan)` traps.
    func testAZeroHeightViewDoesNotTrap() {
        XCTAssertEqual(VisualizerRenderer.zoneForY(0, height: 0, numZones: 5), 0)
        XCTAssertEqual(VisualizerRenderer.zoneForY(10, height: 0, numZones: 5), 0)
        XCTAssertEqual(VisualizerRenderer.zoneForY(10, height: -5, numZones: 5), 0)
    }

    func testNonNumbersDoNotTrap() {
        XCTAssertEqual(VisualizerRenderer.zoneForY(.nan, height: 100, numZones: 5), 0)
        XCTAssertEqual(VisualizerRenderer.zoneForY(.infinity, height: 100, numZones: 5), 0)
        XCTAssertEqual(VisualizerRenderer.zoneForY(10, height: .infinity, numZones: 5), 0)
    }

    /// The callers build `max(numZones, 1)` zone paths, so even no zones
    /// must index the one path there is.
    func testNoZonesStillIndexesTheOnePath() {
        XCTAssertEqual(VisualizerRenderer.zoneForY(50, height: 100, numZones: 0), 0)
        XCTAssertEqual(VisualizerRenderer.zoneForY(50, height: 100, numZones: -3), 0)
    }

    func testEveryResultIsAValidIndex() {
        for zones in 1...8 {
            for y in stride(from: -20.0, through: 220.0, by: 7.5) {
                let zone = VisualizerRenderer.zoneForY(CGFloat(y), height: 200, numZones: zones)
                XCTAssertTrue((0..<zones).contains(zone), "y \(y), \(zones) zones gave \(zone)")
            }
        }
    }

    func testZoneColours() {
        let colours: [Color] = [.red, .green, .blue]
        XCTAssertEqual(VisualizerRenderer.zoneColor(0, in: colours), .red)
        XCTAssertEqual(VisualizerRenderer.zoneColor(2, in: colours), .blue)
        XCTAssertEqual(VisualizerRenderer.zoneColor(9, in: colours), .blue, "past the end: the last colour")
        XCTAssertEqual(VisualizerRenderer.zoneColor(-1, in: colours), .red)
    }

    /// `colors[min(zone, colors.count - 1)]` asked an empty list for -1.
    func testNoColoursDoesNotCrash() {
        XCTAssertEqual(VisualizerRenderer.zoneColor(0, in: []), .green)
        XCTAssertEqual(VisualizerRenderer.zoneColor(3, in: []), .green)
    }
}
