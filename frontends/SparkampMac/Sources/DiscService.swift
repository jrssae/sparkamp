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
    /// Core's one wording for the loaded-media state, decoded from the FFI
    /// payload's `media_summary` — the same string GTK/TUI show.
    var mediaSummaryCore: String?

    private enum CodingKeys: String, CodingKey {
        case id, label, media, toc, mountPath
        case mediaSummaryCore = "mediaSummary"
    }

    /// One-line loaded-media state: the core's wording, with a local
    /// fallback only for payloads that predate the field.
    var mediaSummary: String {
        if let s = mediaSummaryCore, !s.isEmpty { return s }
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

/// The fixed CDDB category set — submissions must use one of these. Fetched
/// once from the core (`gnudb::CATEGORIES`), so it can't drift.
let gnudbCategories: [String] = DiscService.categories()

/// Every ID3v1 genre, alphabetically sorted — the genre typeahead items.
/// Fetched once from the core (`ID3V1_GENRES`).
let id3GenreList: [String] = DiscService.id3Genres()

/// Best-effort genre → CDDB category — the core's `gnudb::suggest_category`.
func suggestGnudbCategory(for genre: String) -> String {
    DiscService.suggestCategory(genre)
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

    /// Fixed CDDB categories from the core. Falls back to the known set if
    /// the FFI somehow fails (never expected).
    static func categories() -> [String] {
        guard let json = takeString(sparkamp_gnudb_categories(nil)),
              let data = json.data(using: .utf8),
              let cats = try? JSONDecoder().decode([String].self, from: data),
              !cats.isEmpty
        else {
            return ["blues", "classical", "country", "data", "folk", "jazz",
                    "misc", "newage", "reggae", "rock", "soundtrack"]
        }
        return cats
    }

    /// Alphabetical ID3v1 genre list from the core (typeahead items).
    static func id3Genres() -> [String] {
        guard let json = takeString(sparkamp_id3_genres(nil)),
              let data = json.data(using: .utf8),
              let genres = try? JSONDecoder().decode([String].self, from: data)
        else { return [] }
        return genres
    }

    /// Core `gnudb::suggest_category` — free-text genre → fixed category.
    static func suggestCategory(_ genre: String) -> String {
        genre.withCString {
            takeString(sparkamp_gnudb_suggest_category(nil, $0)) ?? "misc"
        }
    }

    // MARK: Rip job (core loop: pre-flight, per-track tags, cancel, frac)

    /// A whole rip selection (`RipRunIn` in Rust).
    struct RipRunJob: Codable {
        var entries: [DiscTrackEntry]
        var destRoot: String
        var quality: Int
        var tags: XmcdEntry
        var totalOnDisc: Int
    }

    /// A finished job's results (`RipJobDone` in Rust).
    struct RipJobDone: Codable {
        var ripped: [String]
        var failures: [String]
        var cancelled: Bool
    }

    /// Poll snapshot (`RipJobStatus` in Rust).
    struct RipJobStatus: Codable {
        var running: Bool
        var trackIndex: Int
        var trackCount: Int
        var title: String
        /// 0–1 within the current track.
        var frac: Double
        var done: RipJobDone?
    }

    /// Start the core rip worker. False when the JSON failed or a rip is
    /// already running.
    static func ripJobStart(job: RipRunJob) -> Bool {
        guard let json = jsonString(job) else { return false }
        return json.withCString { sparkamp_disc_rip_job_start(nil, $0) == 0 }
    }

    /// Poll the running/just-finished job (call from a main-thread timer).
    static func ripJobPoll() -> RipJobStatus? {
        guard let json = takeString(sparkamp_disc_rip_job_poll(nil)),
              let data = json.data(using: .utf8)
        else { return nil }
        return try? decoder().decode(RipJobStatus.self, from: data)
    }

    /// Stop the running job after the current track.
    static func ripJobCancel() {
        sparkamp_disc_rip_job_cancel(nil)
    }

    /// The shared one-line rip result for a finished job.
    static func ripResultMessage(done: RipJobDone, imported: Int) -> String {
        guard let json = jsonString(done) else { return "Rip finished" }
        return json.withCString {
            takeString(sparkamp_disc_rip_result_message(nil, $0, Int32(imported)))
                ?? "Rip finished"
        }
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

    /// Probe durations for a batch of absolute file paths (GStreamer
    /// discovery per file — runs synchronously, so call this from a
    /// background queue, same as every other DiscService entry point).
    /// A path missing from the result, or mapped to `nil`, is unreadable.
    static func probeDurations(paths: [String]) -> [String: UInt32?] {
        guard let json = jsonString(paths) else { return [:] }
        let out = json.withCString { sparkamp_disc_probe_durations(nil, $0) }
        struct Probe: Codable { let path: String; let secs: UInt32? }
        guard let text = takeString(out),
              let data = text.data(using: .utf8),
              let probes = try? decoder().decode([Probe].self, from: data)
        else { return [:] }
        return Dictionary(uniqueKeysWithValues: probes.map { ($0.path, $0.secs) })
    }

    /// A whole burn job (`BurnRunIn` in Rust). The caller has already done
    /// the pre-flight (capacity, eraseDecision, the erase confirmation).
    struct BurnRunJob: Codable {
        var drive: OpticalDrive
        var items: [BurnJobItem]
        var audio: Bool
        var useM3u: Bool
        var eraseFirst: Bool
        var verify: Bool
    }

    /// One queued file: path + the display line the phase messages show.
    struct BurnJobItem: Codable {
        var path: String
        var display: String
    }

    /// A finished job (`BurnJobDone` in Rust): success flag + status line.
    struct BurnJobDone: Codable {
        var ok: Bool
        var message: String
    }

    /// Poll snapshot (`BurnJobStatus` in Rust).
    struct BurnJobStatus: Codable {
        var running: Bool
        var phase: String
        var done: BurnJobDone?
    }

    /// Start the core burn worker (staging, optional erase, prep, burn,
    /// cleanup — the same job GTK/TUI run). False when the JSON failed or a
    /// burn is already running.
    static func burnJobStart(job: BurnRunJob) -> Bool {
        guard let json = jsonString(job) else { return false }
        return json.withCString { sparkamp_disc_burn_job_start(nil, $0) == 0 }
    }

    /// Poll the running/just-finished burn (call from a main-thread timer).
    static func burnJobPoll() -> BurnJobStatus? {
        guard let json = takeString(sparkamp_disc_burn_job_poll(nil)),
              let data = json.data(using: .utf8)
        else { return nil }
        return try? decoder().decode(BurnJobStatus.self, from: data)
    }

    /// Cancel the burn: stops between steps and kills a mid-write subprocess.
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
