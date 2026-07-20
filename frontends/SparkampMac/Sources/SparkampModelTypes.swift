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

// MARK: - Granite catalog

/// Display names for the Granite effects, index-aligned with the core's
/// `GraniteEffect` numbering (`sparkamp_get/set_granite_effect`). One shared
/// list: the Settings dropdown and the fullscreen FPS overlay must agree.
enum GraniteCatalog {
    static let effectNames = [
        "Plasma", "Tunnel", "Swirl", "Spin", "Cells", "Explode",
        "Ripple", "Shear", "Kaleidoscope", "Gravity Well", "Drain", "Flag",
    ]

    /// Name for a core effect index; tolerant of out-of-range values.
    static func effectName(_ index: Int) -> String {
        effectNames.indices.contains(index) ? effectNames[index] : "Effect \(index)"
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
    /// Cached/resolved artwork file path (A2 thumbnail column); empty if `hasArt` is false.
    let artworkPath: String
    // Phase-1 technical fields (Task 3/7)
    /// Sample rate in Hz; 0 if unknown.
    let sampleRate: Int
    /// File size in bytes; 0 if unknown.
    let fileSize: Int64
    /// ISO-8601 UTC timestamp of the row's first INSERT; empty if unknown.
    let addedAt: String
    /// ISO-8601 UTC timestamp of the file's on-disk modification time; empty if unknown.
    let fileMtime: String
    /// "VBR" / "CBR" for MP3 files; empty when undetermined or non-MP3.
    let bitrateMode: String
    /// Channel count (1 = mono, 2 = stereo, ...); 0 if unknown. Crosses the
    /// FFI alongside the five Task 7 fields — needed for the ID3 tech line.
    let channels: Int

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

    /// Human-friendly local rendering of addedAt, or "" if unknown. Same
    /// ISO8601DateFormatter -> DateFormatter pattern as lastPlayedDisplay —
    /// GTK's format_last_played reformats added_at too (ml_columns.rs
    /// :385-394), not just last_played, so mac must match here as well.
    var addedAtDisplay: String {
        guard !addedAt.isEmpty else { return "" }
        let inFmt = ISO8601DateFormatter()
        guard let date = inFmt.date(from: addedAt) else { return addedAt }
        let outFmt = DateFormatter()
        outFmt.dateFormat = "yyyy-MM-dd HH:mm"
        return outFmt.string(from: date)
    }

    /// Human-friendly local rendering of fileMtime, or "" if unknown. Same
    /// pattern as lastPlayedDisplay / addedAtDisplay — GTK reformats
    /// file_mtime through format_last_played too.
    var fileMtimeDisplay: String {
        guard !fileMtime.isEmpty else { return "" }
        let inFmt = ISO8601DateFormatter()
        guard let date = inFmt.date(from: fileMtime) else { return fileMtime }
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
        artworkPath = ""
        sampleRate = 0
        fileSize = 0
        addedAt = ""
        fileMtime = ""
        bitrateMode = ""
        channels = 0
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
        artworkPath = cBytesToString(&c.artwork_path)
        sampleRate  = Int(c.sample_rate)
        fileSize    = c.file_size
        addedAt     = cBytesToString(&c.added_at)
        fileMtime   = cBytesToString(&c.file_mtime)
        bitrateMode = cBytesToString(&c.bitrate_mode)
        channels    = Int(c.channels)
    }
}

// MARK: - Now Playing (A1 panel / A6 art window data)

/// Swift-side mirror of the core's `NowPlayingInfo` (`sparkamp_now_playing_*`
/// FFI, `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h`). Built fresh
/// from `SparkampModel.refreshNowPlaying()` on every track change; `nil` on
/// the model means nothing is playing (`sparkamp_now_playing_open` returned
/// NULL).
struct NowPlayingInfo {
    /// Curated, non-empty tag rows in core order (title/artist/album first).
    let tags: [(String, String)]
    /// e.g. "MP3 · 320kbps · 44.1kHz · Stereo · 3:45"; empty if unprobed.
    let techLine: String
    /// Resolved artwork file path; empty if none.
    let artworkPath: String
    /// True if the track is indexed in the media library (play count known).
    let hasPlayCount: Bool
    /// Valid only when `hasPlayCount` is true.
    let playCount: Int64
    /// ISO-8601 UTC timestamp; empty if never played / unindexed.
    let lastPlayed: String
    /// Wikipedia search URL for the artist tag; empty if the tag is empty.
    let artistWikiURL: String
    /// Wikipedia search URL for the album tag; empty if the tag is empty.
    let albumWikiURL: String
}

// MARK: - Media Library playlist item

struct MLPlaylistItem: Identifiable {
    let id: Int64   // DB row id — stable key for CRUD operations
    let name: String
    /// File path of the playlist file on disk (.m3u8, or legacy .m3u).
    var path: String = ""
}

// MARK: - Device types

/// Cached song / playlist counts for a connected device, keyed by device id.
struct DeviceCounts: Equatable {
    var songs: Int
    var playlists: Int
}

/// Live progress of a copy-to-device operation ("done/total · filename"),
/// driving the detail view's copy progress bar.
struct CopyProgress: Equatable {
    var done: Int
    var total: Int
    var name: String
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

struct PlaylistItem: Identifiable, Equatable {
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
