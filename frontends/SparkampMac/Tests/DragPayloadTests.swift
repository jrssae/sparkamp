import XCTest
import AppKit
import UniformTypeIdentifiers

/// Track drags: the tracklist format on the pasteboard, the round trip
/// through an item provider, the in-process payload, and which rows a drag
/// carries.
@MainActor
final class DragPayloadTests: XCTestCase {

    private let awkwardPaths = [
        "/Music/The Test Pressing/Placeholder/02 Lorem Ipsum (Dolor Sit Amet).mp3",
        "/Music/Mötley Test/Café — 東京.flac",
        "/Music/Odd/line one\nline two.mp3",   // macOS allows a newline in a name
        "/Music/Odd/  spaced  .mp3",
    ]

    // MARK: The tracklist format

    func testTracklistRoundTripsEveryPath() {
        let data = TrackDragPayload.encode(awkwardPaths)
        XCTAssertEqual(TrackDragPayload.decode(data), awkwardPaths)
    }

    func testANewlineInAFileNameIsOnePathNotTwo() {
        let path = "/Music/Odd/line one\nline two.mp3"
        XCTAssertEqual(TrackDragPayload.decode(TrackDragPayload.encode([path])), [path])
    }

    func testEmptyTracklists() {
        XCTAssertEqual(TrackDragPayload.decode(TrackDragPayload.encode([])), [])
        XCTAssertEqual(TrackDragPayload.decode(Data()), [])
        XCTAssertEqual(TrackDragPayload.decode(Data("a\0\0b\0".utf8)), ["a", "b"], "empties are dropped")
    }

    // MARK: Through an item provider

    private func resolve(_ providers: [NSItemProvider]) -> [String] {
        let done = expectation(description: "resolved")
        var result: [String] = []
        TrackDragPayload.resolvePaths(from: providers) { paths in
            result = paths
            done.fulfill()
        }
        wait(for: [done], timeout: 5)
        return result
    }

    func testAProviderCarriesEveryPathOfAMultiRowDrag() {
        let provider = TrackDragPayload.provider(forPaths: awkwardPaths)
        XCTAssertTrue(provider.hasItemConformingToTypeIdentifier(kSparkampTracklistUTI))
        XCTAssertTrue(provider.hasItemConformingToTypeIdentifier(UTType.fileURL.identifier),
                      "Finder gets the first file")
        XCTAssertEqual(resolve([provider]), awkwardPaths)
    }

    /// The tracklist type is registered even with no paths: a deferred
    /// payload has none at gesture time, and without the type the drag
    /// would not be droppable at all.
    func testAnEmptyDragIsStillADragButCarriesNothing() {
        let provider = TrackDragPayload.provider(forPaths: [])
        XCTAssertTrue(provider.hasItemConformingToTypeIdentifier(kSparkampTracklistUTI))
        XCTAssertFalse(provider.hasItemConformingToTypeIdentifier(UTType.fileURL.identifier))
        XCTAssertEqual(resolve([provider]), [])
    }

    func testAFileDraggedInFromFinderResolvesToItsPath() throws {
        let url = URL(fileURLWithPath: "/Music/From Finder/Exhibit B.mp3")
        let provider = NSItemProvider()
        let data = url.dataRepresentation
        provider.registerDataRepresentation(forTypeIdentifier: UTType.fileURL.identifier,
                                            visibility: .all) { completion in
            completion(data, nil)
            return nil
        }
        XCTAssertEqual(resolve([provider]), [url.path])
    }

    func testSeveralProvidersAreAllResolved() {
        let a = TrackDragPayload.provider(forPaths: ["/a.mp3", "/b.mp3"])
        let b = TrackDragPayload.provider(forPaths: ["/c.mp3"])
        XCTAssertEqual(Set(resolve([a, b])), ["/a.mp3", "/b.mp3", "/c.mp3"])
    }

    // MARK: The in-process payload

    func testAParkedPayloadIsTakenOnce() {
        _ = SparkampDrag.take()   // whatever an earlier test left
        _ = SparkampDrag.begin(.paths(["/a.mp3"]), pasteboardPaths: ["/a.mp3"])
        guard case .paths(let paths)? = SparkampDrag.take() else {
            return XCTFail("the payload was not parked")
        }
        XCTAssertEqual(paths, ["/a.mp3"])
        XCTAssertNil(SparkampDrag.take(), "a drop must not reuse a drag's payload")
    }

    func testANewDragReplacesALeftoverOne() {
        _ = SparkampDrag.begin(.libraryIds([1, 2]))
        _ = SparkampDrag.begin(.libraryIds([7]))
        guard case .libraryIds(let ids)? = SparkampDrag.take() else {
            return XCTFail("expected library ids")
        }
        XCTAssertEqual(ids, [7])
    }

    // MARK: Which rows a drag carries

    private struct Row: Identifiable, Equatable { let id: Int }

    func testDraggingASelectedRowCarriesTheWholeSelectionInListOrder() {
        let items = (1...5).map(Row.init)
        let rows = SparkampDrag.rows(Row(id: 4), in: items, selection: [4, 2, 5])
        XCTAssertEqual(rows.map(\.id), [2, 4, 5])
    }

    func testDraggingAnUnselectedRowCarriesOnlyThatRow() {
        let items = (1...5).map(Row.init)
        XCTAssertEqual(SparkampDrag.rows(Row(id: 1), in: items, selection: [4, 2]).map(\.id), [1])
        XCTAssertEqual(SparkampDrag.rows(Row(id: 3), in: items, selection: []).map(\.id), [3])
    }
}
