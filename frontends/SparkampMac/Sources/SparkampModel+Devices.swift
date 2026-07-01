import Foundation

// MARK: - Device list + counts + detail operations
//
// State lives in SparkampModel (extensions can't hold stored properties).
//
// THREADING: the device FFI ops are now self-contained (each opens its own
// short-lived DB connection — see open_lib in src/ffi/devices.rs), so the
// heavy ones (browse / copy / sync / scan / send) run on a background queue
// and hop back to the main actor to publish results — the UI never freezes and
// the spinners actually animate. Light ops (volume refresh, the count-only
// dir walk) stay on the main actor.

extension SparkampModel {

    /// Preferred playlist format (0 = m3u8, 1 = m3u), read on the main actor and
    /// passed into the device playlist ops so they don't touch ctx off-main.
    private var playlistFormat: Int32 {
        ctx.map { sparkamp_get_playlist_format($0) } ?? 0
    }

    /// Enumerate volumes and refresh the canonical device list (marker-file IO
    /// only, fast — fine on the main actor at the 2 s poll cadence). Prunes
    /// stale count cache + selection, then tops up counts.
    func pollDevices() {
        let volumes = DeviceService.enumerateVolumes()
        let fresh = DeviceService.refresh(volumes: volumes)

        if fresh != devices {
            devices = fresh
            let liveIds = Set(fresh.map { $0.id })
            deviceCounts = deviceCounts.filter { liveIds.contains($0.key) }
        }
        if let sel = selectedDeviceBSD,
           !devices.contains(where: { $0.backendId == sel }) {
            selectedDeviceBSD = nil
        }
        refreshDeviceCounts()
    }

    /// Count songs/playlists for any not-yet-counted device. The count-only FFI
    /// is a directory walk (no tag reads, no SQLite), cheap enough for the main
    /// actor; cached per device id so this is a no-op once filled.
    func refreshDeviceCounts() {
        for dev in devices where deviceCounts[dev.id] == nil && dev.fsVisible {
            deviceCounts[dev.id] = DeviceService.counts(device: dev)
        }
    }

    /// Force-refresh counts for one device (after a copy/sync changes contents).
    func refreshDeviceCounts(for dev: Device) {
        deviceCounts[dev.id] = DeviceService.counts(device: dev)
    }

    // MARK: Detail-view operations (background)

    /// Load the device's audio files (with "synced from") into `deviceTracks`.
    func loadDeviceTracks(_ device: Device) {
        guard device.fsVisible else { deviceTracks = []; return }
        DispatchQueue.global(qos: .userInitiated).async {
            let tracks = DeviceService.browse(device: device)
            DispatchQueue.main.async { self.deviceTracks = tracks }
        }
    }

    /// Copy library files onto the device under Music/<file>, recording sync
    /// pairs, with live per-file progress. Each file copies on the background
    /// queue (UI stays responsive) and the progress bar updates on the main
    /// actor between files.
    func copyToDevice(_ device: Device, paths: [String]) {
        guard !paths.isEmpty, !deviceBusy else { return }
        deviceBusy = true
        deviceStatus = nil
        copyProgress = CopyProgress(done: 0, total: paths.count, name: "")
        copyNextFile(device, paths: paths, index: 0, copied: 0, skipped: 0)
    }

    private func copyNextFile(
        _ device: Device, paths: [String], index: Int, copied: Int, skipped: Int
    ) {
        if index >= paths.count {
            // Done — refresh the file list (background) then publish.
            DispatchQueue.global(qos: .userInitiated).async {
                let tracks = device.fsVisible ? DeviceService.browse(device: device) : []
                DispatchQueue.main.async {
                    self.deviceTracks = tracks
                    self.refreshDeviceCounts(for: device)
                    self.deviceBusy = false
                    self.copyProgress = nil
                    self.deviceStatus =
                        "Copied \(copied)\(skipped > 0 ? " · skipped \(skipped)" : "")"
                }
            }
            return
        }
        let path = paths[index]
        copyProgress = CopyProgress(
            done: index, total: paths.count,
            name: URL(fileURLWithPath: path).lastPathComponent)
        DispatchQueue.global(qos: .userInitiated).async {
            let r = DeviceService.copy(device: device, srcPaths: [path])
            DispatchQueue.main.async {
                self.copyNextFile(
                    device, paths: paths, index: index + 1,
                    copied: copied + (r?.copied ?? 0),
                    skipped: skipped + (r?.skipped ?? 0))
            }
        }
    }

    /// Two-way sync the device. Fetches the plan off-thread; if it has no
    /// both-changed conflicts the auto changes apply immediately, otherwise the
    /// plan is stashed and the conflict-resolution sheet is presented (the apply
    /// then happens in `resolveSyncConflicts`).
    func syncDevice(_ device: Device) {
        guard !deviceBusy else { return }
        deviceBusy = true
        deviceStatus = nil
        DispatchQueue.global(qos: .userInitiated).async {
            guard let plan = DeviceService.syncPlan(device: device) else {
                DispatchQueue.main.async {
                    self.deviceBusy = false
                    self.deviceStatus = "Sync failed"
                }
                return
            }
            if plan.conflicts.isEmpty {
                let result = DeviceService.applySync(device: device, plan: plan, choices: [])
                let tracks = device.fsVisible ? DeviceService.browse(device: device) : []
                let applied = result?.applied ?? 0
                DispatchQueue.main.async {
                    self.deviceTracks = tracks
                    self.refreshDeviceCounts(for: device)
                    self.deviceBusy = false
                    self.deviceStatus = "Synced \(applied) change\(applied == 1 ? "" : "s")"
                }
            } else {
                // Hand off to the sheet; nothing is applied until the user acts.
                DispatchQueue.main.async {
                    self.deviceBusy = false
                    self.pendingSyncDevice = device
                    self.pendingSyncPlan = plan
                }
            }
        }
    }

    /// Apply the pending two-way sync after the conflict sheet closes. `choices`
    /// carries the user's per-song picks; an empty array (Cancel) still applies
    /// the auto pairs and skips the conflicts. Clears the pending plan, then
    /// refreshes the file list + counts.
    func resolveSyncConflicts(choices: [ConflictChoice]) {
        guard let device = pendingSyncDevice, let plan = pendingSyncPlan else { return }
        pendingSyncPlan = nil
        pendingSyncDevice = nil
        guard !deviceBusy else { return }
        deviceBusy = true
        deviceStatus = nil
        DispatchQueue.global(qos: .userInitiated).async {
            let result = DeviceService.applySync(device: device, plan: plan, choices: choices)
            let tracks = device.fsVisible ? DeviceService.browse(device: device) : []
            let applied = result?.applied ?? 0
            let skipped = result?.skipped ?? 0
            DispatchQueue.main.async {
                self.deviceTracks = tracks
                self.refreshDeviceCounts(for: device)
                self.deviceBusy = false
                self.deviceStatus = skipped > 0
                    ? "Synced \(applied) · skipped \(skipped)"
                    : "Synced \(applied) change\(applied == 1 ? "" : "s")"
            }
        }
    }

    /// Send a whole library playlist to the device (its tracks + the .m3u).
    func sendPlaylistToDevice(_ device: Device, playlistId: Int64) {
        guard !deviceBusy else { return }
        let format = playlistFormat
        deviceBusy = true
        deviceStatus = nil
        DispatchQueue.global(qos: .userInitiated).async {
            let r = DeviceService.sendPlaylist(
                device: device, playlistId: playlistId, format: format)
            let tracks = device.fsVisible ? DeviceService.browse(device: device) : []
            DispatchQueue.main.async {
                self.deviceTracks = tracks
                self.refreshDeviceCounts(for: device)
                self.deviceBusy = false
                self.deviceStatus = r.ok
                    ? "Sent playlist · copied \(r.copied)"
                    : "Couldn't send playlist"
            }
        }
    }

    /// Re-read the device's files from disk.
    func scanDevice(_ device: Device) {
        guard !deviceBusy else { return }
        deviceBusy = true
        deviceStatus = nil
        DispatchQueue.global(qos: .userInitiated).async {
            let tracks = device.fsVisible ? DeviceService.browse(device: device) : []
            DispatchQueue.main.async {
                self.deviceTracks = tracks
                self.refreshDeviceCounts(for: device)
                self.deviceBusy = false
                self.deviceStatus =
                    "Scanned \(tracks.count) file\(tracks.count == 1 ? "" : "s")"
            }
        }
    }

    // MARK: Device playlists

    /// Load the device's playlists into `devicePlaylists` (background).
    func loadDevicePlaylists(_ device: Device) {
        guard device.fsVisible else { devicePlaylists = []; return }
        DispatchQueue.global(qos: .userInitiated).async {
            let pls = DeviceService.devicePlaylists(device: device)
            DispatchQueue.main.async { self.devicePlaylists = pls }
        }
    }

    func newDevicePlaylist(_ device: Device, name: String) {
        runDevicePlaylistOp(device) { DeviceService.playlistNew(device: device, name: name, format: $0) }
    }

    func renameDevicePlaylist(_ device: Device, relpath: String, newName: String) {
        runDevicePlaylistOp(device) {
            DeviceService.playlistRename(device: device, relpath: relpath, newName: newName, format: $0)
        }
    }

    func duplicateDevicePlaylist(_ device: Device, relpath: String) {
        runDevicePlaylistOp(device) { _ in
            DeviceService.playlistDuplicate(device: device, relpath: relpath)
        }
    }

    func deleteDevicePlaylist(_ device: Device, relpath: String) {
        runDevicePlaylistOp(device) { _ in
            DeviceService.playlistDelete(device: device, relpath: relpath)
        }
    }

    /// Run a device-playlist file op on the background queue (passing the
    /// playlist format), then reload the playlist list + counts on the main
    /// actor. `op` returns whether it succeeded.
    private func runDevicePlaylistOp(_ device: Device, _ op: @escaping (Int32) -> Bool) {
        guard !deviceBusy else { return }
        let format = playlistFormat
        deviceBusy = true
        DispatchQueue.global(qos: .userInitiated).async {
            let ok = op(format)
            let pls = DeviceService.devicePlaylists(device: device)
            DispatchQueue.main.async {
                self.devicePlaylists = pls
                self.refreshDeviceCounts(for: device)
                self.deviceBusy = false
                if !ok { self.deviceStatus = "Playlist operation failed" }
            }
        }
    }

    /// Remove files from ONE device playlist's .m3u (the files stay on the
    /// device) — the "Remove" action, distinct from "Delete". Reloads the
    /// playlists (their entries changed) so the chip filter updates.
    func removeFromDevicePlaylist(_ device: Device, relpath: String, paths: [String]) {
        guard !paths.isEmpty, !deviceBusy else { return }
        deviceBusy = true
        deviceStatus = nil
        DispatchQueue.global(qos: .userInitiated).async {
            let ok = DeviceService.playlistRemoveEntries(
                device: device, relpath: relpath, paths: paths)
            let pls = DeviceService.devicePlaylists(device: device)
            DispatchQueue.main.async {
                self.devicePlaylists = pls
                self.deviceBusy = false
                self.deviceStatus = ok
                    ? "Removed \(paths.count) from playlist"
                    : "Couldn't remove from playlist"
            }
        }
    }

    /// Permanently delete files from the device (caller confirmed), then refresh.
    func deleteFromDevice(_ device: Device, paths: [String]) {
        guard !paths.isEmpty, !deviceBusy else { return }
        deviceBusy = true
        deviceStatus = nil
        DispatchQueue.global(qos: .userInitiated).async {
            let failed = DeviceService.deleteFiles(device: device, paths: paths)
            let tracks = device.fsVisible ? DeviceService.browse(device: device) : []
            DispatchQueue.main.async {
                self.deviceTracks = tracks
                self.refreshDeviceCounts(for: device)
                self.deviceBusy = false
                let deleted = paths.count - max(failed, 0)
                self.deviceStatus = failed > 0
                    ? "Deleted \(deleted) · \(failed) failed"
                    : "Deleted \(deleted) file\(deleted == 1 ? "" : "s")"
            }
        }
    }

    /// Eject a device with in-flight feedback. Marks it ejecting (drives the
    /// detail spinner), then on the DiskArbitration callback clears the flag and
    /// either re-polls (success — the device drops off the list, and the detail
    /// view auto-falls back to the overview) or surfaces an error (busy).
    func ejectDevice(_ device: Device) {
        let bsd = device.backendId
        guard !bsd.isEmpty, !ejectingDevices.contains(bsd) else { return }
        ejectingDevices.insert(bsd)
        ejectError = nil
        DeviceService.eject(bsdName: bsd) { ok in
            Task { @MainActor in
                self.ejectingDevices.remove(bsd)
                if ok {
                    self.pollDevices()
                } else {
                    let name = device.label.isEmpty ? "the device" : device.label
                    self.ejectError =
                        "Couldn't eject \(name). Close any apps using it and try again."
                }
            }
        }
    }
}
