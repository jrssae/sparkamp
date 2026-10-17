import XCTest

/// Text the Swift side formats itself: durations, the status line, track
/// names, file sizes, timestamps, and the C strings that cross the bridge.
@MainActor
final class FormattingTests: XCTestCase {

    // MARK: Status line

    /// `playlistStatusLine` is a Swift copy of the core's
    /// `playlist_status_line` and says it must match EXACTLY. These are the
    /// core's own test cases (src/playlist_status.rs), so a change to either
    /// side that the other did not get fails here.
    func testStatusLineMatchesTheCoresCases() {
        XCTAssertEqual(playlistStatusLine(count: 1, totalSecs: 65, selected: nil),
                       "1 track · 1:05 total")
        XCTAssertEqual(playlistStatusLine(count: 12, totalSecs: 2900, selected: nil),
                       "12 tracks · 48:20 total")
        XCTAssertEqual(playlistStatusLine(count: 12, totalSecs: 3665, selected: (3, 664)),
                       "12 tracks · 1:01:05 total · 3 selected · 11:04")
        XCTAssertEqual(playlistStatusLine(count: 5, totalSecs: 100, selected: (1, 65)),
                       "5 tracks · 1:40 total · 1 selected · 1:05")
        XCTAssertEqual(playlistStatusLine(count: 0, totalSecs: 0, selected: nil),
                       "0 tracks · 0:00 total")
    }

    func testStatusLineRollsOverToHoursExactlyAtAnHour() {
        XCTAssertEqual(playlistStatusLine(count: 2, totalSecs: 3599, selected: nil),
                       "2 tracks · 59:59 total")
        XCTAssertEqual(playlistStatusLine(count: 2, totalSecs: 3600, selected: nil),
                       "2 tracks · 1:00:00 total")
        XCTAssertEqual(playlistStatusLine(count: 2, totalSecs: 36_000, selected: (0, 0)),
                       "2 tracks · 10:00:00 total · 0 selected · 0:00")
    }

    // MARK: Durations

    func testDurations() {
        XCTAssertEqual(formatDuration(0), "0:00")
        XCTAssertEqual(formatDuration(59.9), "0:59", "seconds truncate, never round up")
        XCTAssertEqual(formatDuration(60), "1:00")
        XCTAssertEqual(formatDuration(3661), "61:01", "minutes do not roll over to hours")
    }

    func testUnknownDurations() {
        XCTAssertEqual(formatDuration(-1), "--:--")
        XCTAssertEqual(formatDuration(.nan), "--:--")
        XCTAssertEqual(formatDuration(.infinity), "--:--", "Int(.infinity) traps")
        XCTAssertEqual(formatDuration(-.infinity), "--:--")
    }

    // MARK: Track names

    func testTrackDisplayName() {
        XCTAssertEqual(trackDisplayName(title: "Hearsay", artist: "Mock Trial", albumArtist: ""),
                       "Mock Trial — Hearsay")
        XCTAssertEqual(trackDisplayName(title: "Hearsay", artist: "", albumArtist: "Various"),
                       "Various — Hearsay")
        XCTAssertEqual(trackDisplayName(title: "Hearsay", artist: "Mock Trial", albumArtist: "Various"),
                       "Mock Trial — Hearsay", "the track artist wins")
        XCTAssertEqual(trackDisplayName(title: "Hearsay", artist: "", albumArtist: ""), "Hearsay")
        XCTAssertEqual(trackDisplayName(title: "", artist: "", albumArtist: ""), "Unknown")
        XCTAssertEqual(trackDisplayName(title: "", artist: "Mock Trial", albumArtist: ""),
                       "Mock Trial — Unknown")
    }

    // MARK: File sizes

    /// Same thresholds as the core's `format_file_size` (decimal units,
    /// whole KB below 1 MB). Unknown sizes are blank here rather than "0 KB".
    func testFileSizes() {
        XCTAssertEqual(MLFilesTable.formatFileSize(0), "")
        XCTAssertEqual(MLFilesTable.formatFileSize(-5), "")
        XCTAssertEqual(MLFilesTable.formatFileSize(999), "0 KB")
        XCTAssertEqual(MLFilesTable.formatFileSize(1_000), "1 KB")
        XCTAssertEqual(MLFilesTable.formatFileSize(999_999), "999 KB")
        XCTAssertEqual(MLFilesTable.formatFileSize(1_000_000), "1.0 MB")
        XCTAssertEqual(MLFilesTable.formatFileSize(3_260_000), "3.3 MB")
        XCTAssertEqual(MLFilesTable.formatFileSize(1_500_000_000), "1500.0 MB")
    }

    // MARK: Timestamps

    func testTimestampsRenderAsLocalMinutes() throws {
        let shown = localTimestampDisplay("2026-09-02T22:14:05Z")
        XCTAssertNotNil(shown.range(of: #"^\d{4}-\d{2}-\d{2} \d{2}:\d{2}$"#, options: .regularExpression),
                        shown)
        // It is the same instant, in local time, to the minute.
        let back = DateFormatter()
        back.dateFormat = "yyyy-MM-dd HH:mm"
        let parsed = try XCTUnwrap(back.date(from: shown))
        let original = try XCTUnwrap(ISO8601DateFormatter().date(from: "2026-09-02T22:14:00Z"))
        XCTAssertEqual(parsed.timeIntervalSince1970, original.timeIntervalSince1970, accuracy: 0.5)
    }

    func testMissingAndUnreadableTimestamps() {
        XCTAssertEqual(localTimestampDisplay(""), "")
        XCTAssertEqual(localTimestampDisplay("yesterday"), "yesterday", "shown as stored")
        XCTAssertEqual(localTimestampDisplay("2026-13-45T99:99:99Z"), "2026-13-45T99:99:99Z")
    }

    func testPlaylistNamesCarryNoColons() {
        let name = defaultTimestampedPlaylistName()
        XCTAssertTrue(name.hasPrefix("Playlist "))
        XCTAssertFalse(name.contains(":"), "a colon is not safe in every file system")
        XCTAssertNotNil(name.range(of: #"^Playlist \d{4}-\d{2}-\d{2} \d{2}-\d{2}$"#,
                                   options: .regularExpression), name)
    }

    // MARK: Small helpers

    func testGraniteEffectNames() {
        XCTAssertEqual(GraniteCatalog.effectName(0), "Plasma")
        XCTAssertEqual(GraniteCatalog.effectName(GraniteCatalog.effectNames.count - 1), "Flag")
        XCTAssertEqual(GraniteCatalog.effectName(-1), "Effect -1")
        XCTAssertEqual(GraniteCatalog.effectName(GraniteCatalog.effectNames.count),
                       "Effect \(GraniteCatalog.effectNames.count)")
        XCTAssertEqual(Set(GraniteCatalog.effectNames).count, GraniteCatalog.effectNames.count,
                       "effect names must be distinct")
    }

    func testClamped() {
        XCTAssertEqual(5.clamped(to: 0...3), 3)
        XCTAssertEqual((-1).clamped(to: 0...3), 0)
        XCTAssertEqual(2.clamped(to: 0...3), 2)
        XCTAssertEqual(1.5.clamped(to: 0...1), 1.0)
    }

    // MARK: C strings from the core

    func testCBytesStopAtTheFirstNul() {
        var bytes: (UInt8, UInt8, UInt8, UInt8) = (0x61, 0x62, 0, 0x63)
        XCTAssertEqual(cBytesToString(&bytes), "ab")
        var empty: (UInt8, UInt8) = (0, 0x41)
        XCTAssertEqual(cBytesToString(&empty), "")
    }

    func testCBytesFillingTheWholeBufferAreAllRead() {
        var bytes: (UInt8, UInt8, UInt8) = (0x61, 0x62, 0x63)
        XCTAssertEqual(cBytesToString(&bytes), "abc")
    }

    /// A leading byte-order mark is kept: the core keeps it, and an album
    /// name looked up with it stripped matched nothing.
    func testCBytesKeepAByteOrderMark() {
        var bytes: (UInt8, UInt8, UInt8, UInt8, UInt8) = (0xEF, 0xBB, 0xBF, 0x41, 0)
        let s = cBytesToString(&bytes)
        XCTAssertEqual(s.unicodeScalars.first, "\u{FEFF}")
        XCTAssertEqual(s, "\u{FEFF}A")
    }

    func testCBytesWithInvalidUTF8StillDecode() {
        var bytes: (UInt8, UInt8, UInt8) = (0x61, 0xFF, 0x62)
        XCTAssertEqual(cBytesToString(&bytes), "a\u{FFFD}b")
    }
}
