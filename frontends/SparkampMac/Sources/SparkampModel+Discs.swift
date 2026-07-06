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

    /// Add disc tracks to the active playlist with their TOC titles and
    /// durations ("Track N" until gnudb supplies real names — Phase 2). No
    /// metadata scan or duration probe: the AIFFs carry no tags and the
    /// durations are already exact.
    ///
    /// Mirrors `mlDoubleClickTracks` semantics: honors the replace/append
    /// add-behavior setting, and autoplay-on-add starts the first new track
    /// when the playlist was replaced or was empty (never interrupts a track
    /// already playing).
    func addDiscTracks(_ entries: [DiscTrackEntry]) {
        guard let ctx = ctx, !entries.isEmpty else { return }
        let shouldReplace = Int(sparkamp_get_playlist_add_behavior(ctx)) == 1
        let autoplay = sparkamp_get_autoplay_on_add(ctx)
        if shouldReplace { clearPlaylist() }
        let indexBefore = Int(sparkamp_playlist_len(ctx))
        let wasEmpty = indexBefore == 0
        for e in entries {
            e.path.withCString { p in
                e.title.withCString { t in
                    _ = sparkamp_playlist_add_entry(ctx, p, t, Int32(e.durationSecs))
                }
            }
        }
        if autoplay && wasEmpty {
            sparkamp_playlist_jump(ctx, Int32(indexBefore))
            sparkamp_play(ctx)
        }
        refreshPlaylist()
        refreshCurrentTrackInfo()
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
