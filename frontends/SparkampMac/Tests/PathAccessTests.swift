import XCTest

/// The Allow Access prompt's inputs. The prompt itself needs a sandboxed app
/// and a person to answer it; the core's own tests cover which folder it asks
/// for. These cover what Swift decides.
@MainActor
final class PathAccessTests: XCTestCase {

    /// Under the App Sandbox the home directory FileManager reports is the
    /// app's container, and an open panel started there shows the wrong
    /// Music folder.
    func testTheMusicFolderIsTheRealOneNotTheContainers() {
        let music = SparkampModel.musicFolder
        XCTAssertEqual(music.lastPathComponent, "Music")
        XCTAssertTrue(music.path.hasPrefix("/"))
        XCTAssertFalse(music.path.contains("/Library/Containers/"), music.path)
        XCTAssertTrue(music.hasDirectoryPath)
    }

    func testNothingToPlayNeedsNoFolder() {
        XCTAssertNil(SparkampModel.folderNeedingAccess([]))
    }

    /// Outside the sandbox every file the user can see opens, so nothing is
    /// ever asked for, and a missing file is never mistaken for a refusal.
    func testReadableAndMissingFilesNeedNoFolder() throws {
        let file = FileManager.default.temporaryDirectory
            .appendingPathComponent("sparkamp-access-\(UUID().uuidString).mp3")
        try Data("x".utf8).write(to: file)
        defer { try? FileManager.default.removeItem(at: file) }
        XCTAssertNil(SparkampModel.folderNeedingAccess([file.path]))
        XCTAssertNil(SparkampModel.folderNeedingAccess(["/nonexistent/sparkamp/gone.mp3"]))
        XCTAssertNil(SparkampModel.folderNeedingAccess([file.path, "", "cdda://1"]))
    }
}
