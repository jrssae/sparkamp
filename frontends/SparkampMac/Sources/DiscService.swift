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

// MARK: - gnudb models

/// One disc gnudb proposed for our TOC (`DiscMatch` in Rust).
struct DiscMatch: Codable, Identifiable, Equatable {
    var category: String
    var discid: String
    var title: String
    var exact: Bool
    var id: String { "\(category)/\(discid)" }
}

/// A parsed gnudb entry (`XmcdEntry` in Rust).
struct XmcdEntry: Codable, Equatable {
    var discid: String
    var artist: String
    var album: String
    var year: String
    var genre: String
    var trackTitles: [String]
    var extd: String
    var extt: [String]
    /// Entry revision from the matched record; a submission updating it must
    /// send revision + 1.
    var revision: Int
}

/// The fixed CDDB category set — submissions must use one of these.
let gnudbCategories = [
    "blues", "classical", "country", "data", "folk", "jazz",
    "misc", "newage", "reggae", "rock", "soundtrack",
]

/// Best-effort genre → CDDB category (mirrors the Rust suggest_category).
func suggestGnudbCategory(for genre: String) -> String {
    let g = genre.lowercased()
    let pairs: [(String, String)] = [
        ("blues", "blues"), ("classic", "classical"), ("country", "country"),
        ("folk", "folk"), ("jazz", "jazz"), ("new age", "newage"),
        ("newage", "newage"), ("reggae", "reggae"),
        ("soundtrack", "soundtrack"), ("rock", "rock"), ("metal", "rock"),
        ("punk", "rock"),
    ]
    for (needle, cat) in pairs where g.contains(needle) { return cat }
    return "misc"
}

/// `{"ok":…}` / `{"error":…}` wrapper the gnudb FFI returns.
private struct GnudbResult<T: Codable>: Codable {
    var ok: T?
    var error: String?
}

/// User-facing gnudb failure ("couldn't reach gnudb: …").
struct GnudbFailure: Error {
    let message: String
}

/// One Burn-list row (mirrors the core BurnItem; lives Swift-side because
/// the queue is frontend state).
struct BurnEntry: Identifiable, Equatable {
    var path: String
    var display: String
    var durationSecs: Int?
    var bytes: UInt64
    var id: String { path }
}

/// The user's tag set for one disc — a gnudb match, hand edits, or both.
/// Keyed by freedb disc ID in the model; feeds display titles now and rip
/// tagging / submission in Phases 3–4.
struct DiscTagSet: Codable, Equatable {
    var artist: String = ""
    var album: String = ""
    var year: String = ""
    var genre: String = ""
    /// Track titles in track order (index 0 = track 1).
    var titles: [String] = []
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

    /// Encode any payload to the snake_case JSON the Rust side expects —
    /// the one place the encoder round-trip lives.
    private static func jsonString<T: Encodable>(_ value: T) -> String? {
        (try? encoder().encode(value)).flatMap { String(data: $0, encoding: .utf8) }
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
        guard let driveJSON = jsonString(drive) else { return [] }
        let out = driveJSON.withCString { sparkamp_disc_track_entries(nil, $0) }
        guard let json = takeString(out),
              let data = json.data(using: .utf8),
              let entries = try? decoder().decode([DiscTrackEntry].self, from: data)
        else { return [] }
        return entries
    }

    /// The freedb disc ID for a TOC — the per-disc key for tag overrides.
    static func discId(toc: DiscToc) -> String? {
        guard let json = jsonString(toc) else { return nil }
        let out = json.withCString { sparkamp_disc_id(nil, $0) }
        return takeString(out)
    }

    /// Ask gnudb which discs match this TOC. Blocking network (10 s timeout)
    /// — background queue only. Returns matches or a user-facing error string.
    static func gnudbQuery(toc: DiscToc, email: String) -> Result<[DiscMatch], GnudbFailure> {
        guard let tocJSON = jsonString(toc)
        else { return .failure(GnudbFailure(message: "bad TOC")) }
        let out = tocJSON.withCString { t in
            email.withCString { e in sparkamp_gnudb_query(nil, t, e) }
        }
        return decodeGnudb(takeString(out))
    }

    /// Fetch + parse one matched entry. Blocking network — background only.
    static func gnudbRead(
        category: String, discid: String, email: String
    ) -> Result<XmcdEntry, GnudbFailure> {
        let out = category.withCString { c in
            discid.withCString { d in
                email.withCString { e in sparkamp_gnudb_read(nil, c, d, e) }
            }
        }
        return decodeGnudb(takeString(out))
    }

    /// The persisted tag record for a disc from the on-disk cache
    /// (disc_tags.toml). File IO — background preferred.
    static func tagsGet(discid: String) -> (user: XmcdEntry?, official: XmcdEntry?) {
        struct Record: Codable {
            var user: XmcdEntry?
            var official: XmcdEntry?
        }
        let out = discid.withCString { sparkamp_disc_tags_get(nil, $0) }
        guard let json = takeString(out),
              let data = json.data(using: .utf8),
              let rec = try? decoder().decode(Record.self, from: data)
        else { return (nil, nil) }
        return (rec.user, rec.official)
    }

    /// Persist a disc's tag record so it survives restarts. File IO —
    /// background preferred.
    @discardableResult
    static func tagsSet(discid: String, user: XmcdEntry, official: XmcdEntry?) -> Bool {
        guard let userJSON = jsonString(user) else { return false }
        let officialJSON = official.flatMap { jsonString($0) }
        return discid.withCString { d in
            userJSON.withCString { u in
                if let oj = officialJSON {
                    return oj.withCString { o in sparkamp_disc_tags_set(nil, d, u, o) }
                }
                return sparkamp_disc_tags_set(nil, d, u, nil)
            }
        }
    }

    /// Validate + POST an entry to gnudb. `entry.revision` must already be
    /// the value to write (matched + 1 for updates, 0 for a new disc).
    /// Blocking network — background only.
    static func gnudbSubmit(
        toc: DiscToc, entry: XmcdEntry, category: String, email: String, testMode: Bool
    ) -> Result<String, GnudbFailure> {
        guard let tocJSON = jsonString(toc), let entryJSON = jsonString(entry)
        else { return .failure(GnudbFailure(message: "bad submit payload")) }
        let out = tocJSON.withCString { t in
            entryJSON.withCString { en in
                category.withCString { c in
                    email.withCString { em in
                        sparkamp_gnudb_submit(nil, t, en, c, em, testMode)
                    }
                }
            }
        }
        return decodeGnudb(takeString(out))
    }

    private static func decodeGnudb<T: Codable>(_ json: String?) -> Result<T, GnudbFailure> {
        guard let json = json, let data = json.data(using: .utf8),
              let wrapped = try? decoder().decode(GnudbResult<T>.self, from: data)
        else { return .failure(GnudbFailure(message: "unreadable gnudb reply")) }
        if let ok = wrapped.ok { return .success(ok) }
        return .failure(GnudbFailure(message: wrapped.error ?? "unknown gnudb error"))
    }

    /// One rip job for `sparkamp_disc_rip_track` (mirrors RipJobIn).
    struct RipJob: Codable {
        struct Source: Codable {
            var kind: String   // "file" on macOS
            var path: String
        }
        var source: Source
        var destRoot: String
        var quality: Int      // 0 = V0, 1 = V2, 2 = 320 CBR
        var discArtist: String
        var album: String
        var year: String
        var genre: String
        var number: Int
        var total: Int
        var title: String
    }

    /// Rip one track to a tagged MP3. Blocks for the whole encode (optical
    /// reads run at drive speed — a minute or more per track): worker thread
    /// only. Returns the written path or a failure message.
    static func ripTrack(job: RipJob) -> Result<String, GnudbFailure> {
        guard let json = jsonString(job)
        else { return .failure(GnudbFailure(message: "bad rip job")) }
        let out = json.withCString { sparkamp_disc_rip_track(nil, $0) }
        return decodeGnudb(takeString(out))
    }

    // MARK: Burning (blind-implemented; hardware pass pending — see plan)

    /// 0 = blank (burn now), 1 = RW with content (erase after explicit
    /// confirmation), 2 = refuse (write-once with content / no media).
    static func eraseDecision(drive: OpticalDrive) -> Int {
        guard let json = jsonString(drive) else { return 2 }
        return Int(json.withCString { sparkamp_disc_erase_decision(nil, $0) })
    }

    /// Audio capacity of the loaded media in seconds.
    static func audioCapacitySecs(drive: OpticalDrive) -> Int {
        guard let json = jsonString(drive) else { return 4800 }
        return Int(json.withCString { sparkamp_disc_audio_capacity_secs(nil, $0) })
    }

    /// Transcode one file to a Red Book WAV (pre-burn step). Blocking —
    /// worker thread; loop per track.
    static func prepareWav(src: String, out: String) -> Result<String, GnudbFailure> {
        let ptr = src.withCString { s in
            out.withCString { o in sparkamp_disc_prepare_wav(nil, s, o) }
        }
        return decodeGnudb(takeString(ptr))
    }

    /// Erase the loaded rewritable disc — only after explicit confirmation.
    static func eraseDisc(drive: OpticalDrive) -> Result<String, GnudbFailure> {
        guard let json = jsonString(drive)
        else { return .failure(GnudbFailure(message: "bad drive")) }
        let ptr = json.withCString { sparkamp_disc_erase(nil, $0) }
        return decodeGnudb(takeString(ptr))
    }

    /// Burn prepared WAVs (track order) as an audio CD. Blocking whole burn.
    static func burnAudio(
        drive: OpticalDrive, stagedDir: String, wavs: [String], verify: Bool
    ) -> Result<String, GnudbFailure> {
        guard let driveJSON = jsonString(drive),
              let wavsJSON = jsonString(wavs)
        else { return .failure(GnudbFailure(message: "bad burn payload")) }
        let ptr = driveJSON.withCString { d in
            stagedDir.withCString { s in
                wavsJSON.withCString { w in
                    sparkamp_disc_burn_audio(nil, d, s, w, verify)
                }
            }
        }
        return decodeGnudb(takeString(ptr))
    }

    /// Stage files, write the MP3-CD companion playlist, and burn as a data
    /// disc. Blocking whole burn.
    static func burnData(
        drive: OpticalDrive, stagedDir: String, files: [String],
        playlistFormat: Int32, verify: Bool
    ) -> Result<String, GnudbFailure> {
        guard let driveJSON = jsonString(drive),
              let filesJSON = jsonString(files)
        else { return .failure(GnudbFailure(message: "bad burn payload")) }
        let ptr = driveJSON.withCString { d in
            stagedDir.withCString { s in
                filesJSON.withCString { f in
                    sparkamp_disc_burn_data(nil, d, s, f, playlistFormat, verify)
                }
            }
        }
        return decodeGnudb(takeString(ptr))
    }

    /// Kill the in-flight burn/erase subprocess.
    static func burnCancel() {
        sparkamp_disc_burn_cancel(nil)
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
