import SwiftUI
import AppKit

// MARK: - Deduplication

extension SparkampModel {
    // MARK: Deduplication

    func startDedup() {
        guard let ctx = ctx, mlIsOpen else { return }
        dedupGroups = []
        dedupRunning = true
        dedupGroupTotal = 0

        let selfAddr = Unmanaged.passUnretained(self).toOpaque()
        let selfAddrInt = Int(bitPattern: selfAddr)

        dedupCtxPtr = sparkamp_dedup_start(ctx,
            // group_cb — called from a Rayon thread
            { ud, groupPtr in
                guard let groupPtr else { return }
                let group = groupPtr.pointee
                var items: [DedupTrackItem] = []
                for i in 0..<Int(group.track_count) {
                    var t = group.tracks[i]
                    let p = cBytesToString(&t.path)
                    items.append(DedupTrackItem(
                        id: p,
                        path: p,
                        title:  cBytesToString(&t.title),
                        artist: cBytesToString(&t.artist),
                        durationSecs: t.duration_secs
                    ))
                }
                let conf = Int(group.confidence)
                let newGroup = DedupGroupItem(id: UUID(), confidence: conf, tracks: items)
                let modelAddr = Int(bitPattern: ud)
                DispatchQueue.main.async {
                    let model = Unmanaged<SparkampModel>
                        .fromOpaque(UnsafeRawPointer(bitPattern: modelAddr)!)
                        .takeUnretainedValue()
                    MainActor.assumeIsolated { model.dedupGroups.append(newGroup) }
                }
            },
            // done_cb
            { ud, totalCount in
                let modelAddr = Int(bitPattern: ud)
                DispatchQueue.main.async {
                    let model = Unmanaged<SparkampModel>
                        .fromOpaque(UnsafeRawPointer(bitPattern: modelAddr)!)
                        .takeUnretainedValue()
                    MainActor.assumeIsolated {
                        model.dedupRunning = false
                        model.dedupGroupTotal = Int(totalCount)
                    }
                }
            },
            UnsafeMutableRawPointer(bitPattern: selfAddrInt)
        )
    }

    func cancelDedup() {
        if let dctx = dedupCtxPtr {
            sparkamp_dedup_cancel(dctx)
        }
    }

    func freeDedup() {
        if let dctx = dedupCtxPtr {
            sparkamp_dedup_free(dctx)
            dedupCtxPtr = nil
        }
    }

    func dedupAddGroupToPlaylist(_ group: DedupGroupItem) {
        guard let ctx = ctx else { return }
        var ptrs: [UnsafePointer<CChar>?] = group.tracks.map { _ in nil }
        let cStrings = group.tracks.map { ($0.path as NSString).utf8String! }
        ptrs = cStrings.map { $0 }
        ptrs.withUnsafeMutableBufferPointer { buf in
            sparkamp_dedup_add_to_playlist(ctx, buf.baseAddress, Int32(group.tracks.count))
        }
        refreshPlaylist()
    }

    func dedupReplacePlaylistWithGroup(_ group: DedupGroupItem) {
        guard let ctx = ctx else { return }
        var ptrs: [UnsafePointer<CChar>?] = group.tracks.map { ($0.path as NSString).utf8String }
        ptrs.withUnsafeMutableBufferPointer { buf in
            sparkamp_dedup_replace_playlist(ctx, buf.baseAddress, Int32(group.tracks.count))
        }
        refreshAll()
    }

    func openInFinder(_ path: String) {
        path.withCString { sparkamp_open_file_location($0) }
    }

}
