import Foundation

// MARK: - Optical disc models (mirror src/disc JSON via snake_case decoding)

enum MediaKind: String, Codable, Equatable {
    case cdR = "CdR"
    case cdRw = "CdRw"
    case dvdR = "DvdR"
    case dvdRw = "DvdRw"
    case dvdRam = "DvdRam"
    case unknown = "Unknown"

    var displayName: String {
        switch self {
        case .cdR: return "CD-R"
        case .cdRw: return "CD-RW"
        case .dvdR: return "DVD-R"
        case .dvdRw: return "DVD-RW"
        case .dvdRam: return "DVD-RAM"
        case .unknown: return "disc"
        }
    }
}

struct DiscMediaInfo: Codable, Equatable {
    var present: Bool
    var isAudioCd: Bool
    var isBlank: Bool
    var rewritable: Bool
    var kind: MediaKind
    var freeBytes: UInt64
    var capacityBytes: UInt64
}

struct DiscTocTrack: Codable, Equatable {
    var number: Int
    var startFrame: UInt32
    var isAudio: Bool
}

struct DiscToc: Codable, Equatable {
    var tracks: [DiscTocTrack]
    var leadoutFrame: UInt32
}

/// One physical optical drive (`OpticalDrive` in Rust). `id` is the drutil
/// drive index on macOS.
struct OpticalDrive: Codable, Identifiable, Equatable {
    var id: String
    var label: String
    var media: DiscMediaInfo
    var toc: DiscToc?
    var mountPath: String?

    /// One-line loaded-media state — mirrors the Rust `media_summary()`.
    var mediaSummary: String {
        if !media.present { return "No disc" }
        if media.isAudioCd {
            let n = toc?.tracks.count ?? 0
            return "Audio CD (\(n) track\(n == 1 ? "" : "s"))"
        }
        if media.isBlank { return "Blank \(media.kind.displayName)" }
        return "Data disc"
    }
}

/// Playlist-ready track entry (`DiscTrackEntry` in Rust).
struct DiscTrackEntry: Codable, Identifiable, Equatable {
    var number: Int
    var path: String
    var title: String
    var durationSecs: UInt32
    var id: Int { number }

    var durationText: String {
        String(format: "%d:%02d", durationSecs / 60, durationSecs % 60)
    }
}

// MARK: - Disc FFI service

/// Thin wrapper over the `sparkamp_disc_*` JSON FFI. All calls are ctx-free
/// (detection runs drutil/plutil subprocesses in the core) — run them on a
/// background queue and publish on the main actor, like DeviceService.
enum DiscService {

    private static func decoder() -> JSONDecoder {
        let d = JSONDecoder()
        d.keyDecodingStrategy = .convertFromSnakeCase
        return d
    }

    private static func encoder() -> JSONEncoder {
        let e = JSONEncoder()
        e.keyEncodingStrategy = .convertToSnakeCase
        return e
    }

    /// Take ownership of a C string returned by the FFI and free it.
    private static func takeString(_ p: UnsafeMutablePointer<CChar>?) -> String? {
        guard let p = p else { return nil }
        defer { sparkamp_free_string(p) }
        return String(cString: p)
    }

    /// Every optical drive with its loaded-media state. Subprocess-backed —
    /// never call on the main thread.
    static func listDrives() -> [OpticalDrive] {
        guard let json = takeString(sparkamp_disc_list_drives(nil)),
              let data = json.data(using: .utf8),
              let drives = try? decoder().decode([OpticalDrive].self, from: data)
        else { return [] }
        return drives
    }

    /// Playlist-ready entries for the drive's audio tracks (empty when the
    /// drive holds no audio disc). Reads the mounted volume — background only.
    static func trackEntries(drive: OpticalDrive) -> [DiscTrackEntry] {
        guard let payload = try? encoder().encode(drive),
              let driveJSON = String(data: payload, encoding: .utf8)
        else { return [] }
        let out = driveJSON.withCString { sparkamp_disc_track_entries(nil, $0) }
        guard let json = takeString(out),
              let data = json.data(using: .utf8),
              let entries = try? decoder().decode([DiscTrackEntry].self, from: data)
        else { return [] }
        return entries
    }

    /// Eject the disc in the given drutil drive (macOS). Runs `drutil eject`
    /// off-thread; `completion(success)` on the main queue.
    static func eject(driveId: String, completion: @escaping (Bool) -> Void) {
        DispatchQueue.global(qos: .userInitiated).async {
            let p = Process()
            p.executableURL = URL(fileURLWithPath: "/usr/bin/drutil")
            p.arguments = ["eject", "-drive", driveId]
            p.standardOutput = FileHandle.nullDevice
            p.standardError = FileHandle.nullDevice
            let ok = (try? p.run()) != nil
            if ok { p.waitUntilExit() }
            DispatchQueue.main.async {
                completion(ok && p.terminationStatus == 0)
            }
        }
    }
}
