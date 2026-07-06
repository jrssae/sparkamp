import Foundation

// MARK: - Optical-disc operations
//
// State lives in SparkampModel (extensions can't hold stored properties).
// Detection and track listing shell out to drutil/plutil inside the core, so
// both always run on a background queue and hop back to the main actor —
// mirroring the device-sync threading model.

extension SparkampModel {

    /// Re-enumerate optical drives (background) and publish changes. Also
    /// clears a stale drive selection the same way pollDevices does.
    func pollDiscDrives() {
        DispatchQueue.global(qos: .utility).async {
            let drives = DiscService.listDrives()
            DispatchQueue.main.async {
                if drives != self.discDrives {
                    self.discDrives = drives
                }
            }
        }
    }

    /// Load the playlist-ready track entries for one drive's disc into
    /// `discTracks` (background; empty when no audio disc).
    func loadDiscTracks(_ drive: OpticalDrive) {
        discBusy = true
        DispatchQueue.global(qos: .userInitiated).async {
            let entries = DiscService.trackEntries(drive: drive)
            DispatchQueue.main.async {
                self.discTracks = entries
                self.discBusy = false
            }
        }
    }

    /// Append disc tracks to the active playlist with their TOC titles and
    /// durations ("Track N" until gnudb supplies real names — Phase 2). No
    /// metadata scan or duration probe: the AIFFs carry no tags and the
    /// durations are already exact.
    func addDiscTracks(_ entries: [DiscTrackEntry]) {
        guard let ctx = ctx, !entries.isEmpty else { return }
        for e in entries {
            e.path.withCString { p in
                e.title.withCString { t in
                    _ = sparkamp_playlist_add_entry(ctx, p, t, Int32(e.durationSecs))
                }
            }
        }
        refreshPlaylist()
        discStatus = "Added \(entries.count) disc track\(entries.count == 1 ? "" : "s")"
    }

    /// Eject the disc in a drive, with in-flight feedback; on success the
    /// next poll drops the mounted volume (and the detail view empties).
    func ejectDisc(_ drive: OpticalDrive) {
        guard !ejectingDiscs.contains(drive.id) else { return }
        ejectingDiscs.insert(drive.id)
        DiscService.eject(driveId: drive.id) { ok in
            self.ejectingDiscs.remove(drive.id)
            if ok {
                self.discTracks = []
                self.pollDiscDrives()
            } else {
                self.discStatus = "Couldn't eject the disc"
            }
        }
    }
}
