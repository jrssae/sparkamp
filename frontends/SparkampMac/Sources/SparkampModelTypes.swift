import Foundation
import AppKit
import IOKit.pwr_mgt

// MARK: - C-array string helper

/// Convert a fixed-size C byte array (imported as a tuple in Swift) to a String.
/// Stops at the first null byte; interprets as UTF-8.
func cBytesToString<T>(_ value: inout T) -> String {
    withUnsafeBytes(of: &value) { bytes in
        let end = bytes.firstIndex(of: 0) ?? bytes.endIndex
        return String(bytes: bytes[..<end], encoding: .utf8) ?? ""
    }
}

// MARK: - Media Library types

/// A single track row from the media library.
struct MLTrack: Identifiable {
    let id: Int64
    let path: String
    let title: String
    let artist: String
    let album: String
    let genre: String
    let year: Int
    let trackNum: Int
    let lengthSecs: Double
    let bitrate: Int
    let playCount: Int
    let scanned: Bool
    // Extended DB fields
    let albumArtist: String
    let discNum: Int
    let bpm: String
    let comment: String
    let composer: String
    let readOnly: Bool
    let hasArt: Bool
    let fileMissing: Bool
    /// ISO-8601 UTC timestamp from the DB ("YYYY-MM-DDTHH:MM:SSZ"); empty if never played.
    let lastPlayed: String

    var durationString: String { formatDuration(lengthSecs) }
    var filename: String { URL(fileURLWithPath: path).lastPathComponent }

    /// Human-friendly local "YYYY-MM-DD HH:MM" rendering of lastPlayed, or "" if never played.
    var lastPlayedDisplay: String {
        guard !lastPlayed.isEmpty else { return "" }
        let inFmt = ISO8601DateFormatter()
        guard let date = inFmt.date(from: lastPlayed) else { return lastPlayed }
        let outFmt = DateFormatter()
        outFmt.dateFormat = "yyyy-MM-dd HH:mm"
        return outFmt.string(from: date)
    }

    /// Stub init for a path that's not in the library DB.  All metadata
    /// fields default to empty / zero.  Used by drop handlers that
    /// receive raw file URLs and need a placeholder row until the next
    /// library scan picks up the file.
    init(stubPath: String) {
        id = 0
        path = stubPath
        title = ""
        artist = ""
        album = ""
        genre = ""
        year = 0
        trackNum = 0
        lengthSecs = 0
        bitrate = 0
        playCount = 0
        scanned = false
        albumArtist = ""
        discNum = 0
        bpm = ""
        comment = ""
        composer = ""
        readOnly = false
        hasArt = false
        fileMissing = !FileManager.default.fileExists(atPath: stubPath)
        lastPlayed = ""
    }

    init(from c: SparkampLibTrack) {
        var c = c
        id          = c.id
        path        = cBytesToString(&c.path)
        title       = cBytesToString(&c.title)
        artist      = cBytesToString(&c.artist)
        album       = cBytesToString(&c.album)
        genre       = cBytesToString(&c.genre)
        year        = Int(c.year)
        trackNum    = Int(c.track_num)
        lengthSecs  = c.length_secs
        bitrate     = Int(c.bitrate)
        playCount   = Int(c.play_count)
        scanned     = c.scanned != 0
        albumArtist = cBytesToString(&c.album_artist)
        discNum     = Int(c.disc_num)
        bpm         = cBytesToString(&c.bpm)
        comment     = cBytesToString(&c.comment)
        composer    = cBytesToString(&c.composer)
        readOnly    = c.read_only != 0
        hasArt      = c.has_art != 0
        fileMissing = c.file_missing != 0
        lastPlayed  = cBytesToString(&c.last_played)
    }
}

// MARK: - Media Library playlist item

struct MLPlaylistItem: Identifiable {
    let id: Int64   // DB row id — stable key for CRUD operations
    let name: String
    /// File path of the playlist file on disk (.m3u8, or legacy .m3u).
    var path: String = ""
}

// MARK: - Dedup types

struct DedupTrackItem: Identifiable {
    let id: String   // path used as stable ID
    let path: String
    let title: String
    let artist: String
    let durationSecs: Double

    var durationString: String { formatDuration(durationSecs) }
    var filename: String { URL(fileURLWithPath: path).lastPathComponent }
}

struct DedupGroupItem: Identifiable {
    let id: UUID
    let confidence: Int   // 0 = Probable, 1 = Less Likely
    let tracks: [DedupTrackItem]

    var confidenceLabel: String { confidence == 0 ? "Probable" : "Less Likely" }
    var label: String {
        guard let first = tracks.first else { return "Unknown" }
        return first.artist.isEmpty ? first.title : "\(first.artist) — \(first.title)"
    }
}

// MARK: - Data types

struct PlaylistItem: Identifiable {
    let id: Int          // the playlist index
    let title: String
    let artist: String
    let albumArtist: String
    let duration: Double // seconds, -1 = unknown
    let broken: Bool
    let readOnly: Bool
    let fileMissing: Bool

    var durationString: String { formatDuration(duration) }

    /// Single-line display string: "Artist — Title" with album_artist fallback.
    var displayName: String { trackDisplayName(title: title, artist: artist, albumArtist: albumArtist) }
}

/// Shared display-name logic used by both the playlist and the marquee.
/// Returns "Artist — Title", falling back to albumArtist when artist is empty,
/// or just the title (which may be the filename stem) when neither is available.
func trackDisplayName(title: String, artist: String, albumArtist: String) -> String {
    let t = title.isEmpty ? "Unknown" : title
    if !artist.isEmpty      { return "\(artist) — \(t)" }
    if !albumArtist.isEmpty { return "\(albumArtist) — \(t)" }
    return t
}

/// Like `trackDisplayName` but for media-library tracks where a `filename`
/// stem is always available.  When both artist and album-artist are blank
/// the row is rendered as just the filename — matching the user-facing
/// expectation for the saved-playlist editor.
func mlTrackDisplay(_ t: MLTrack) -> String {
    let name = t.title.isEmpty ? t.filename : t.title
    if !t.artist.isEmpty      { return "\(t.artist) — \(name)" }
    if !t.albumArtist.isEmpty { return "\(t.albumArtist) — \(name)" }
    return t.filename
}

func formatDuration(_ secs: Double) -> String {
    guard secs >= 0 else { return "--:--" }
    let total = Int(secs)
    let m = total / 60
    let s = total % 60
    return String(format: "%d:%02d", m, s)
}


// MARK: - Comparable clamping helper

extension Comparable {
    func clamped(to range: ClosedRange<Self>) -> Self {
        min(max(self, range.lowerBound), range.upperBound)
    }
}
