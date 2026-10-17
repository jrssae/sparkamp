import XCTest

/// The Swift half of calls into the core that need no running app: JSON
/// marshaling, and the lists and rules Swift takes from the core.
@MainActor
final class BridgeTests: XCTestCase {

    // MARK: JSON marshaling

    private struct Probe: Codable, Equatable {
        var trackNum: Int
        var albumArtist: String
        var rgTrackGain: Double?
    }

    /// The core speaks snake_case; Swift properties are camelCase.
    func testEncodingWritesSnakeCase() throws {
        let json = try XCTUnwrap(SparkampFFI.encodeJSON(Probe(trackNum: 3, albumArtist: "Mock Trial", rgTrackGain: nil)))
        XCTAssertTrue(json.contains("\"track_num\":3"), json)
        XCTAssertTrue(json.contains("\"album_artist\":\"Mock Trial\""), json)
        XCTAssertFalse(json.contains("trackNum"), json)
    }

    func testDecodingReadsSnakeCase() {
        let probe: Probe? = SparkampFFI.decodeJSON(#"{"track_num":7,"album_artist":"Beta Version","rg_track_gain":-6.5}"#)
        XCTAssertEqual(probe, Probe(trackNum: 7, albumArtist: "Beta Version", rgTrackGain: -6.5))
    }

    func testRoundTrip() {
        let original = Probe(trackNum: 10, albumArtist: "The Test Pressing", rgTrackGain: 0)
        let back: Probe? = SparkampFFI.decodeJSON(SparkampFFI.encodeJSON(original))
        XCTAssertEqual(back, original)
    }

    func testBadJSONDecodesToNothingRatherThanCrashing() {
        XCTAssertNil(SparkampFFI.decodeJSON(nil) as Probe?)
        XCTAssertNil(SparkampFFI.decodeJSON("") as Probe?)
        XCTAssertNil(SparkampFFI.decodeJSON("not json") as Probe?)
        XCTAssertNil(SparkampFFI.decodeJSON(#"{"track_num":"three","album_artist":"x"}"#) as Probe?,
                     "a wrong type")
        XCTAssertNil(SparkampFFI.decodeJSON(#"{"album_artist":"x"}"#) as Probe?, "a missing field")
    }

    func testTakingANullStringGivesNothing() {
        XCTAssertNil(SparkampFFI.takeString(nil))
    }

    // MARK: Lists and rules from the core

    /// `DiscService.categories()` falls back to a Swift copy of this list if
    /// the core ever fails to answer. This keeps that copy honest.
    func testCDDBCategoriesMatchTheSwiftFallback() {
        XCTAssertEqual(DiscService.categories(),
                       ["blues", "classical", "country", "data", "folk", "jazz",
                        "misc", "newage", "reggae", "rock", "soundtrack"])
    }

    func testEverySuggestedCategoryIsARealOne() {
        let categories = Set(DiscService.categories())
        for genre in ["Rock", "Indie Rock", "Death Metal", "Punk", "Jazz", "Smooth Jazz",
                      "Classical", "New Age", "Soundtrack", "Synthpop", "", "Ambient"] {
            let suggested = DiscService.suggestCategory(genre)
            XCTAssertTrue(categories.contains(suggested), "\(genre) → \(suggested)")
        }
        XCTAssertEqual(DiscService.suggestCategory("Indie Rock"), "rock")
        XCTAssertEqual(DiscService.suggestCategory("Synthpop"), "misc")
    }

    func testID3GenresAreTheAlphabeticalTypeaheadList() {
        let genres = DiscService.id3Genres()
        XCTAssertFalse(genres.isEmpty)
        XCTAssertTrue(genres.contains("Rock"))
        // The core sorts by ASCII-lowercased name.
        let keys = genres.map { $0.lowercased() }
        XCTAssertEqual(keys, keys.sorted(), "the typeahead list is alphabetical")
        XCTAssertEqual(Set(genres).count, genres.count, "no duplicates")
    }

    /// macOS rips lossless through AVFoundation; there is no quality to pick.
    func testMacRipsAreLosslessFLAC() {
        XCTAssertEqual(DiscService.ripFormatName, "FLAC")
        XCTAssertFalse(DiscService.ripHasQuality)
    }

    func testFileSystemsThatCannotBeWrittenReliably() {
        for fs in ["ntfs", "NTFS", "exfat", "ExFAT"] {
            XCTAssertTrue(DeviceService.fsUnsupported(fs), fs)
        }
        for fs in ["apfs", "hfs", "msdos", ""] {
            XCTAssertFalse(DeviceService.fsUnsupported(fs), fs)
        }
    }
}
