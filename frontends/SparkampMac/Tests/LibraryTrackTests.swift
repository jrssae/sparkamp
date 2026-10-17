import XCTest

/// Library rows as they cross the bridge (`SparkampLibTrack` to `MLTrack`),
/// what a row shows, and the Files table's columns and sorting.
@MainActor
final class LibraryTrackTests: XCTestCase {

    /// Write `s` into a fixed C char array the way the core does: UTF-8,
    /// NUL-terminated, cut to fit.
    private func put<T>(_ s: String, into field: inout T) {
        withUnsafeMutableBytes(of: &field) { buffer in
            buffer.initializeMemory(as: UInt8.self, repeating: 0)
            let bytes = Array(s.utf8.prefix(buffer.count - 1))
            buffer.copyBytes(from: bytes)
        }
    }

    private func track(
        id: Int64 = 1, path: String = "/Music/Mock Trial/Dummy Run/03 Hearsay.mp3",
        title: String = "Hearsay", artist: String = "Mock Trial", album: String = "Dummy Run",
        length: Double = 194, year: Int32 = 2024, playCount: Int32 = 0,
        rgGain: Double = 0, rgAnalyzed: Bool = false
    ) -> MLTrack {
        var c = SparkampLibTrack()
        c.id = id
        put(path, into: &c.path)
        put(title, into: &c.title)
        put(artist, into: &c.artist)
        put(album, into: &c.album)
        put("Alternative", into: &c.genre)
        put("Mock Trial", into: &c.album_artist)
        put("2026-09-02T22:14:05Z", into: &c.last_played)
        put("Variable", into: &c.bitrate_mode)
        c.year = year
        c.track_num = 3
        c.length_secs = length
        c.bitrate = 128
        c.play_count = playCount
        c.scanned = 1
        c.disc_num = 1
        c.has_art = 1
        c.sample_rate = 44_100
        c.file_size = 3_100_000
        c.channels = 2
        c.rg_track_gain = rgGain
        c.rg_analyzed = rgAnalyzed ? 1 : 0
        return MLTrack(from: c)
    }

    // MARK: Crossing the bridge

    func testEveryFieldCrossesTheBridge() {
        let t = track()
        XCTAssertEqual(t.id, 1)
        XCTAssertEqual(t.path, "/Music/Mock Trial/Dummy Run/03 Hearsay.mp3")
        XCTAssertEqual(t.title, "Hearsay")
        XCTAssertEqual(t.artist, "Mock Trial")
        XCTAssertEqual(t.album, "Dummy Run")
        XCTAssertEqual(t.albumArtist, "Mock Trial")
        XCTAssertEqual(t.genre, "Alternative")
        XCTAssertEqual(t.year, 2024)
        XCTAssertEqual(t.trackNum, 3)
        XCTAssertEqual(t.discNum, 1)
        XCTAssertEqual(t.lengthSecs, 194)
        XCTAssertEqual(t.bitrate, 128)
        XCTAssertTrue(t.scanned)
        XCTAssertTrue(t.hasArt)
        XCTAssertFalse(t.readOnly)
        XCTAssertFalse(t.fileMissing)
        XCTAssertEqual(t.sampleRate, 44_100)
        XCTAssertEqual(t.fileSize, 3_100_000)
        XCTAssertEqual(t.channels, 2)
        XCTAssertEqual(t.bitrateMode, "Variable")
        XCTAssertEqual(t.lastPlayed, "2026-09-02T22:14:05Z")
    }

    func testUnicodeSurvivesTheBridge() {
        let t = track(title: "Café Déjà Vu — 東京", artist: "Mötley Test")
        XCTAssertEqual(t.title, "Café Déjà Vu — 東京")
        XCTAssertEqual(t.artist, "Mötley Test")
    }

    // MARK: What a row shows

    func testDisplayProperties() {
        let t = track()
        XCTAssertEqual(t.filename, "03 Hearsay.mp3")
        XCTAssertEqual(t.durationString, "3:14")
        XCTAssertEqual(t.lastPlayedDisplay, localTimestampDisplay("2026-09-02T22:14:05Z"))
        XCTAssertEqual(t.addedAtDisplay, "", "no timestamp, no text")
        XCTAssertEqual(t.fileMtimeDisplay, "")
    }

    /// A gain of 0.0 dB is a real measurement; only "not analysed" is blank.
    func testReplayGainShowsOnlyOnceAnalysed() {
        XCTAssertEqual(track(rgGain: -6.24, rgAnalyzed: false).rgGainDisplay, "")
        XCTAssertEqual(track(rgGain: -6.24, rgAnalyzed: true).rgGainDisplay, "-6.2 dB")
        XCTAssertEqual(track(rgGain: 0, rgAnalyzed: true).rgGainDisplay, "0.0 dB")
        XCTAssertEqual(track(rgGain: 3.06, rgAnalyzed: true).rgGainDisplay, "3.1 dB")
    }

    func testAStubRowForAFileOutsideTheLibrary() throws {
        let missing = MLTrack(stubPath: "/nonexistent/sparkamp/Exhibit B.mp3")
        XCTAssertEqual(missing.id, 0)
        XCTAssertEqual(missing.filename, "Exhibit B.mp3")
        XCTAssertTrue(missing.fileMissing)
        XCTAssertEqual(missing.durationString, "0:00")

        let file = FileManager.default.temporaryDirectory
            .appendingPathComponent("sparkamp-stub-\(UUID().uuidString).mp3")
        try Data("x".utf8).write(to: file)
        defer { try? FileManager.default.removeItem(at: file) }
        XCTAssertFalse(MLTrack(stubPath: file.path).fileMissing)
    }

    // MARK: Columns and sorting

    func testColumnIdentifiersBitsAndSortKeysAreUnique() {
        let specs = MLFilesTable.specs
        XCTAssertEqual(Set(specs.map(\.id)).count, specs.count, "column ids")
        let bits = specs.map(\.bit).filter { $0 >= 0 }
        XCTAssertEqual(Set(bits).count, bits.count, "show/hide mask bits")
        XCTAssertTrue(bits.allSatisfy { $0 < 64 }, "the mask is 64 bits wide")
        let keys = specs.compactMap(\.sortKey)
        XCTAssertEqual(Set(keys).count, keys.count, "sort keys")
    }

    /// A column that offers sorting must be able to sort. `position` is the
    /// playlist editor's own play order and is handled there.
    func testEverySortableColumnHasAComparator() {
        for spec in MLFilesTable.specs {
            guard let key = spec.sortKey, key != "position" else { continue }
            XCTAssertNotNil(MLFilesTable.keyPathComparator(forSortKey: key, ascending: true),
                            "\(spec.id) sorts by \(key) but nothing compares it")
        }
    }

    func testUnknownSortKeysCompareNothing() {
        XCTAssertNil(MLFilesTable.keyPathComparator(forSortKey: "", ascending: true))
        XCTAssertNil(MLFilesTable.keyPathComparator(forSortKey: "Title", ascending: true))
        XCTAssertNil(MLFilesTable.keyPathComparator(forSortKey: "path", ascending: true))
    }

    func testSortingByTitleBothWays() throws {
        let rows = [track(id: 1, title: "Hearsay"), track(id: 2, title: "Exhibit B"),
                    track(id: 3, title: "Stand-In")]
        let up = try XCTUnwrap(MLFilesTable.keyPathComparator(forSortKey: "title", ascending: true))
        let down = try XCTUnwrap(MLFilesTable.keyPathComparator(forSortKey: "title", ascending: false))
        XCTAssertEqual(rows.sorted(using: up).map(\.title), ["Exhibit B", "Hearsay", "Stand-In"])
        XCTAssertEqual(rows.sorted(using: down).map(\.title), ["Stand-In", "Hearsay", "Exhibit B"])
    }

    func testSortingByNumbersIsNumeric() throws {
        let rows = [track(id: 1, length: 600), track(id: 2, length: 65), track(id: 3, length: 194)]
        let byLength = try XCTUnwrap(MLFilesTable.keyPathComparator(forSortKey: "duration", ascending: true))
        XCTAssertEqual(rows.sorted(using: byLength).map(\.id), [2, 3, 1], "65 < 194 < 600, not text order")

        let plays = [track(id: 1, playCount: 10), track(id: 2, playCount: 9), track(id: 3, playCount: 100)]
        let byPlays = try XCTUnwrap(MLFilesTable.keyPathComparator(forSortKey: "play_count", ascending: false))
        XCTAssertEqual(plays.sorted(using: byPlays).map(\.id), [3, 1, 2])
    }
}
