import Foundation
import AppKit
import DiskArbitration

// MARK: - Codable models (mirror the Rust serde JSON in src/ffi/devices.rs)
//
// Field names are snake_case on the wire; the shared encoder/decoder below use
// convert*SnakeCase so Swift can keep camelCase. Enum *values* (e.g. "Udisks",
// "Computer") are not key-converted, so the raw strings must match Rust exactly.

enum DeviceBackend: String, Codable {
    case udisks = "Udisks"
    case mtp = "Mtp"
    case unsupported = "Unsupported"
}

/// A connected external storage device, as the core sees it.
struct Device: Codable, Identifiable, Equatable {
    let id: String          // volume UUID or marker-file id; "" when unpairable
    var label: String
    var mountPath: String
    var fsType: String
    var totalBytes: UInt64
    var freeBytes: UInt64
    var readOnly: Bool
    var ejectable: Bool
    var backendId: String   // BSD name on macOS — used for eject
    var backend: DeviceBackend
    var fsVisible: Bool

    /// Fraction of capacity that is free (0…1); 0 when capacity is unknown.
    var freeFraction: Double {
        totalBytes > 0 ? Double(freeBytes) / Double(totalBytes) : 0
    }
}

/// One audio file on a device, with the library path it was synced from.
struct DeviceTrack: Codable, Identifiable {
    var path: String
    var title: String
    var artist: String
    var album: String
    var albumArtist: String
    var genre: String
    var composer: String
    var comment: String
    var bpm: String
    var year: Int
    var trackNum: Int
    var discNum: Int
    var lengthSecs: Double
    var bitrate: Int
    var playCount: Int
    var lastPlayed: String
    var hasArt: Bool
    var syncedFrom: String?

    var id: String { path }
}

/// A single auto-resolved pair in a sync plan (SyncPairDto).
struct SyncPair: Codable, Identifiable {
    var libPath: String
    var devPath: String
    var fieldSummary: String
    var id: String { devPath }
}

/// One field that differs between the two copies of a song.
struct FieldDiff: Codable, Identifiable {
    var label: String
    var computer: String
    var device: String
    var id: String { label }
}

/// The library/device sync-pair record carried by a conflict (Rust SyncPair).
struct ConflictPair: Codable {
    var deviceId: String
    var deviceRelpath: String
    var libraryPath: String
    var baselineTagHash: String
    var baselineRating: Int
    var baselinePlaycount: Int
    var lastSyncAt: String?
}

/// One both-changed song needing the user to choose (TagConflictItem).
struct ConflictItem: Codable, Identifiable {
    var pair: ConflictPair
    var song: String
    var diffs: [FieldDiff]
    var id: String { pair.deviceRelpath }
}

/// The flat two-way sync plan (SyncPlanDto).
struct SyncPlan: Codable {
    var toDevice: [SyncPair]
    var toLibrary: [SyncPair]
    var conflicts: [ConflictItem]
    var bytesToCopy: UInt64
}

/// Which side the user kept for a conflict (matches Rust KeepSide).
enum KeepSide: String, Codable {
    case computer = "Computer"
    case device = "Device"
}

/// The user's resolution for one conflict, echoed back on apply.
struct ConflictChoice: Codable {
    var devPath: String   // matches ConflictItem.pair.deviceRelpath
    var keep: KeepSide
}

/// One library playlist's sync decision against a device (PlaylistSyncItem).
struct PlaylistSyncItem: Codable, Identifiable {
    var libraryPlaylistId: Int64
    var libraryName: String
    var libraryPath: String
    var deviceId: String
    var deviceFile: String?
    var desiredDeviceFilename: String
    var srcs: [String]
    var devBasenames: [String]
    var dir: String        // "None" | "Push" | "Pull" | "Conflict"
    var differ: Int
    var id: Int64 { libraryPlaylistId }
}

struct ApplyResult: Codable { var applied: Int; var skipped: Int }
struct CopyResult: Codable { var copied: Int; var skipped: Int; var bytes: UInt64 }
struct PlaylistApplyResult: Codable { var pushed: Int; var pulled: Int; var skipped: Int }

/// One removable volume Swift enumerated, sent to the core for canonicalization.
struct VolumeInfo: Encodable {
    var mountPath: String
    var label: String
    var fsType: String
    var bsdName: String
    var totalBytes: UInt64
    var freeBytes: UInt64
    var readOnly: Bool
    var ejectable: Bool
    var volumeUuid: String?
}

// MARK: - DeviceService

/// Bridges the device FFI to Swift. Volume enumeration and eject are pure
/// AppKit/DiskArbitration (no core involvement); the JSON wrappers drive
/// `src/ffi/devices.rs`.
///
/// THREADING: every wrapper here is `ctx`-free — the core opens its own
/// short-lived DB connection per call (open_lib in src/ffi/devices.rs), kept
/// safe against the main-thread tick's connection by SQLite WAL + busy timeout.
/// So `SparkampModel+Devices` runs the heavy ops (browse/copy/sync/scan/send)
/// on a background queue and hops to the main actor only to publish results;
/// nothing here captures the non-Sendable `ctx`.
enum DeviceService {

    private static let decoder: JSONDecoder = {
        let d = JSONDecoder()
        d.keyDecodingStrategy = .convertFromSnakeCase
        return d
    }()
    private static let encoder: JSONEncoder = {
        let e = JSONEncoder()
        e.keyEncodingStrategy = .convertToSnakeCase
        return e
    }()

    // MARK: FFI string helpers

    /// Take ownership of a core-returned C string, freeing it after copying.
    private static func takeString(_ ptr: UnsafeMutablePointer<CChar>?) -> String? {
        guard let ptr = ptr else { return nil }
        defer { sparkamp_free_string(ptr) }
        return String(cString: ptr)
    }

    private static func encodeJSON<T: Encodable>(_ value: T) -> String? {
        guard let data = try? encoder.encode(value) else { return nil }
        return String(data: data, encoding: .utf8)
    }

    private static func decodeJSON<T: Decodable>(_ s: String?) -> T? {
        guard let s = s, let data = s.data(using: .utf8) else { return nil }
        return try? decoder.decode(T.self, from: data)
    }

    private static func deviceJSON(_ device: Device) -> String? { encodeJSON(device) }

    // MARK: Volume enumeration (pure Swift; safe off the main thread)

    /// Enumerate removable/ejectable volumes under /Volumes, skipping the boot
    /// disk. Capacity + read-only come from URLResourceValues; fs type, BSD
    /// name and volume UUID from DiskArbitration.
    static func enumerateVolumes() -> [VolumeInfo] {
        let keys: [URLResourceKey] = [
            .volumeNameKey, .volumeIsRemovableKey, .volumeIsEjectableKey,
            .volumeTotalCapacityKey, .volumeAvailableCapacityKey, .volumeIsReadOnlyKey,
            .volumeIsInternalKey,
        ]
        let urls = FileManager.default.mountedVolumeURLs(
            includingResourceValuesForKeys: keys,
            options: [.skipHiddenVolumes]
        ) ?? []

        let session = DASessionCreate(kCFAllocatorDefault)
        var out: [VolumeInfo] = []
        for url in urls {
            guard let vals = try? url.resourceValues(forKeys: Set(keys)) else { continue }
            let removable = (vals.volumeIsRemovable ?? false) || (vals.volumeIsEjectable ?? false)
            // Unknown "internal" defaults to true (skip): during a Finder eject
            // the boot volume can momentarily report nil resource values, which
            // a `?? false` would let through as a phantom "Macintosh HD" device.
            let internalVol = vals.volumeIsInternal ?? true
            // A real device is removable, not internal, AND mounted under
            // /Volumes (the boot + system volumes mount at / and /System/...,
            // and a /Volumes/Macintosh HD firmlink is internal). All three
            // guards together exclude the boot disk in steady state and during
            // mount/unmount transitions.
            guard removable, !internalVol, url.path.hasPrefix("/Volumes/") else { continue }

            var fsType = ""
            var bsd = ""
            var uuid: String? = nil
            var mediaWritable = true
            if let session = session,
               let disk = DADiskCreateFromVolumePath(kCFAllocatorDefault, session, url as CFURL) {
                if let bsdC = DADiskGetBSDName(disk) { bsd = String(cString: bsdC) }
                if let desc = DADiskCopyDescription(disk) as? [String: Any] {
                    fsType = desc[kDADiskDescriptionVolumeKindKey as String] as? String ?? ""
                    if let raw = desc[kDADiskDescriptionVolumeUUIDKey as String] {
                        // The value is a CFUUID; render it as a string.
                        let cf = raw as! CFUUID
                        uuid = CFUUIDCreateString(kCFAllocatorDefault, cf) as String
                    }
                    // Media-level writability catches a write-locked SD card whose
                    // volume still mounts read-write — but ONLY when the card
                    // reader actually reports the adapter's write-protect notch
                    // to the OS; many readers ignore it. CFBoolean bridges as
                    // Bool or NSNumber depending on context, so try both.
                    let mw = desc[kDADiskDescriptionMediaWritableKey as String]
                    if let w = mw as? Bool {
                        mediaWritable = w
                    } else if let n = mw as? NSNumber {
                        mediaWritable = n.boolValue
                    }
                }
            }

            out.append(VolumeInfo(
                mountPath: url.path,
                label: vals.volumeName ?? url.lastPathComponent,
                fsType: fsType,
                bsdName: bsd,
                totalBytes: UInt64(vals.volumeTotalCapacity ?? 0),
                freeBytes: UInt64(vals.volumeAvailableCapacity ?? 0),
                readOnly: (vals.volumeIsReadOnly ?? false) || !mediaWritable,
                ejectable: vals.volumeIsEjectable ?? false,
                volumeUuid: uuid
            ))
        }
        return out
    }

    /// Holds the DA session + completion alive across the async unmount→eject
    /// callbacks (passed through the C `context` pointer).
    private final class EjectOp {
        let session: DASession
        let completion: (Bool) -> Void
        init(session: DASession, completion: @escaping (Bool) -> Void) {
            self.session = session
            self.completion = completion
        }
    }

    /// Unmount every volume on the device, then eject it. `completion(true)` on
    /// success, called on the main thread.
    ///
    /// Eject must act on the **whole disk** (DADiskEject on a partition object
    /// fails), and the unmount uses the whole-disk option so every volume
    /// detaches first — the previous version operated on the partition with nil
    /// callbacks and never actually ejected. Callbacks are chained through a
    /// retained `EjectOp` context; the session is driven by a dispatch queue
    /// (no run-loop spin needed).
    static func eject(bsdName: String, completion: @escaping (Bool) -> Void = { _ in }) {
        guard !bsdName.isEmpty, let session = DASessionCreate(kCFAllocatorDefault) else {
            DispatchQueue.main.async { completion(false) }
            return
        }
        DASessionSetDispatchQueue(session, DispatchQueue(label: "dev.sparkamp.eject"))
        guard let volDisk = bsdName.withCString({
            DADiskCreateFromBSDName(kCFAllocatorDefault, session, $0)
        }) else {
            DispatchQueue.main.async { completion(false) }
            return
        }
        let wholeDisk = DADiskCopyWholeDisk(volDisk) ?? volDisk
        let op = EjectOp(session: session, completion: completion)
        let ctx = Unmanaged.passRetained(op).toOpaque()

        DADiskUnmount(wholeDisk, DADiskUnmountOptions(kDADiskUnmountOptionWhole), { disk, dissenter, context in
            guard let context = context else { return }
            if dissenter != nil {
                let op = Unmanaged<EjectOp>.fromOpaque(context).takeRetainedValue()
                DispatchQueue.main.async { op.completion(false) }
                return
            }
            DADiskEject(disk, DADiskEjectOptions(kDADiskEjectOptionDefault), { _, dissenter2, context in
                guard let context = context else { return }
                let op = Unmanaged<EjectOp>.fromOpaque(context).takeRetainedValue()
                DispatchQueue.main.async { op.completion(dissenter2 == nil) }
            }, context)
        }, ctx)
    }

    // MARK: JSON FFI wrappers
    //
    // These device ops are `ctx`-free: the core opens its own short-lived DB
    // connection per call (see open_lib in src/ffi/devices.rs), so we pass a
    // nil ctx and the model can run them on a background queue without sharing
    // the non-Sendable ctx pointer. The playlist ops take the playlist format
    // (0 = m3u8, 1 = m3u) the model read on the main thread.

    static func refresh(volumes: [VolumeInfo]) -> [Device] {
        guard let json = encodeJSON(volumes) else { return [] }
        let out = json.withCString { sparkamp_devices_refresh(nil, $0) }
        return decodeJSON(takeString(out)) ?? []
    }

    static func browse(device: Device) -> [DeviceTrack] {
        guard let dj = deviceJSON(device) else { return [] }
        let out = dj.withCString { sparkamp_device_browse(nil, $0) }
        return decodeJSON(takeString(out)) ?? []
    }

    /// Song / playlist counts — a directory walk only, no tag reads (unlike
    /// `browse`).
    static func counts(device: Device) -> DeviceCounts {
        guard let dj = deviceJSON(device) else { return DeviceCounts(songs: 0, playlists: 0) }
        let out = dj.withCString { sparkamp_device_counts(nil, $0) }
        struct Raw: Decodable { var songs: Int; var playlists: Int }
        let raw: Raw? = decodeJSON(takeString(out))
        return DeviceCounts(songs: raw?.songs ?? 0, playlists: raw?.playlists ?? 0)
    }

    static func syncPlan(device: Device) -> SyncPlan? {
        guard let dj = deviceJSON(device) else { return nil }
        let out = dj.withCString { sparkamp_device_sync_plan(nil, $0) }
        return decodeJSON(takeString(out))
    }

    static func applySync(
        device: Device, plan: SyncPlan, choices: [ConflictChoice]
    ) -> ApplyResult? {
        guard let dj = deviceJSON(device),
              let pj = encodeJSON(plan),
              let cj = encodeJSON(choices) else { return nil }
        let out = dj.withCString { d in pj.withCString { p in cj.withCString { c in
            sparkamp_device_apply_sync(nil, d, p, c)
        } } }
        return decodeJSON(takeString(out))
    }

    static func copy(device: Device, srcPaths: [String]) -> CopyResult? {
        guard let dj = deviceJSON(device), let sj = encodeJSON(srcPaths) else { return nil }
        let out = dj.withCString { d in sj.withCString { s in
            sparkamp_device_copy(nil, d, s)
        } }
        return decodeJSON(takeString(out))
    }

    static func playlistPlan(device: Device, format: Int32) -> [PlaylistSyncItem] {
        guard let dj = deviceJSON(device) else { return [] }
        let out = dj.withCString { sparkamp_device_playlist_plan(nil, $0, format) }
        return decodeJSON(takeString(out)) ?? []
    }

    static func playlistApply(device: Device, format: Int32) -> PlaylistApplyResult? {
        guard let dj = deviceJSON(device) else { return nil }
        let out = dj.withCString { sparkamp_device_playlist_apply(nil, $0, format) }
        return decodeJSON(takeString(out))
    }

    /// Send one library playlist (by DB id) to the device — copy its tracks +
    /// write the device .m3u. Returns (copied, ok).
    static func sendPlaylist(
        device: Device, playlistId: Int64, format: Int32
    ) -> (copied: Int, ok: Bool) {
        guard let dj = deviceJSON(device) else { return (0, false) }
        let out = dj.withCString { sparkamp_device_send_playlist(nil, $0, playlistId, format) }
        struct Raw: Decodable { var copied: Int; var ok: Bool }
        let raw: Raw? = decodeJSON(takeString(out))
        return (raw?.copied ?? 0, raw?.ok ?? false)
    }

    /// Permanently delete files from the device. Returns the count that could
    /// NOT be deleted, or -1 on bad input. The CALLER must confirm first.
    static func deleteFiles(device: Device, paths: [String]) -> Int {
        guard let dj = deviceJSON(device), let pj = encodeJSON(paths) else { return -1 }
        return Int(dj.withCString { d in pj.withCString { p in
            sparkamp_device_delete_files(nil, d, p)
        } })
    }

    /// Embedded artwork for one side of a conflict (side: 0 = computer, 1 = device).
    static func conflictArtwork(
        device: Device, devRelpath: String, side: Int
    ) -> NSImage? {
        guard let dj = deviceJSON(device) else { return nil }
        var len: Int32 = 0
        let ptr = dj.withCString { d in devRelpath.withCString { r in
            sparkamp_device_conflict_artwork(nil, d, r, Int32(side), &len)
        } }
        guard let ptr = ptr, len > 0 else { return nil }
        defer { sparkamp_tag_free_artwork(ptr, len) }
        return NSImage(data: Data(bytes: ptr, count: Int(len)))
    }

    /// Whether the filesystem is not reliably writable (NTFS/exFAT) — drives the
    /// unsupported-filesystem badge.
    static func fsUnsupported(_ fsType: String) -> Bool {
        fsType.withCString { sparkamp_device_fs_unsupported($0) }
    }
}
