import Foundation

// MARK: - Device list + counts
//
// State lives in SparkampModel (extensions can't hold stored properties); the
// polling and count logic lives here. All calls run on the main actor — the
// device FFI touches the ctx's SQLite connection, which the tick also uses (see
// DeviceService's THREADING note). Volume enumeration is the only background
// step, and it returns plain Codable structs.

extension SparkampModel {

    /// Enumerate volumes and refresh the canonical device list. Cheap enough to
    /// run on the main actor at the 2 s poll cadence (a handful of volumes,
    /// marker-file reads only — no SQLite). Drops count-cache entries for
    /// devices that went away, and clears a stale selection.
    func pollDevices() {
        guard let ctx = ctx else { return }
        let volumes = DeviceService.enumerateVolumes()
        let fresh = DeviceService.refresh(ctx: ctx, volumes: volumes)

        // Only publish when something actually changed, so we don't re-render
        // the sidebar/overview every 2 s.
        if fresh != devices {
            devices = fresh
            // Prune cached counts for devices no longer present.
            let liveIds = Set(fresh.map { $0.id })
            deviceCounts = deviceCounts.filter { liveIds.contains($0.key) }
        }

        // If the selected device unplugged, fall back to the overview.
        if let sel = selectedDeviceBSD,
           !devices.contains(where: { $0.backendId == sel }) {
            selectedDeviceBSD = nil
        }

        // Compute counts for any device not yet counted. Doing this here (not
        // only in the view's onAppear) is what makes a poll-discovered device's
        // "Counting…" flip to a real number — onAppear fires once, before the
        // poll finds the device. The cache guard keeps this a no-op afterward.
        refreshDeviceCounts()
    }

    /// Compute song / playlist counts for any connected device missing from the
    /// cache. Called when the overview/detail appears. Uses the count-only FFI
    /// (a directory walk, no per-file tag reads), so it does NOT lock up the UI
    /// the way a full `browse` would on a device with many files. Cached by
    /// device id so it runs once per plug-in.
    func refreshDeviceCounts() {
        guard let ctx = ctx else { return }
        for dev in devices where deviceCounts[dev.id] == nil && dev.fsVisible {
            deviceCounts[dev.id] = DeviceService.counts(ctx: ctx, device: dev)
        }
    }

    /// Force-refresh counts for one device (after a copy/sync changes its
    /// contents).
    func refreshDeviceCounts(for dev: Device) {
        guard let ctx = ctx else { return }
        deviceCounts[dev.id] = DeviceService.counts(ctx: ctx, device: dev)
    }

    // MARK: Detail-view operations
    //
    // THREADING: browse/copy/sync touch the ctx's SQLite connection (not Send,
    // shared with the tick), so they run on the main actor. To let SwiftUI
    // paint the busy state first, the work is deferred one run-loop turn via
    // DispatchQueue.main.async before the (blocking) FFI call. For a normal
    // stick this is instant; a device with thousands of files will hitch.
    // True off-main device IO needs a separate Rust DB connection (a later
    // change), mirroring how the library scan already threads.

    /// Load the device's audio files (with "synced from") into `deviceTracks`.
    func loadDeviceTracks(_ device: Device) {
        guard let ctx = ctx, device.fsVisible else { deviceTracks = []; return }
        deviceTracks = DeviceService.browse(ctx: ctx, device: device)
    }

    /// Copy library files onto the device under Music/<file>, recording sync
    /// pairs, with live per-file progress. Each file is copied in its own
    /// deferred main-thread step so SwiftUI repaints the progress bar between
    /// files (a single batched FFI call would block the whole copy with no
    /// visible movement). Refreshes the file list + counts when done.
    func copyToDevice(_ device: Device, paths: [String]) {
        guard ctx != nil, !paths.isEmpty, !deviceBusy else { return }
        deviceBusy = true
        deviceStatus = nil
        copyProgress = CopyProgress(done: 0, total: paths.count, name: "")
        copyNextFile(device, paths: paths, index: 0, copied: 0, skipped: 0)
    }

    private func copyNextFile(
        _ device: Device, paths: [String], index: Int, copied: Int, skipped: Int
    ) {
        guard let ctx = ctx else { return }
        if index >= paths.count {
            loadDeviceTracks(device)
            refreshDeviceCounts(for: device)
            deviceBusy = false
            copyProgress = nil
            deviceStatus = "Copied \(copied)\(skipped > 0 ? " · skipped \(skipped)" : "")"
            return
        }
        let path = paths[index]
        copyProgress = CopyProgress(
            done: index, total: paths.count,
            name: URL(fileURLWithPath: path).lastPathComponent)
        // Defer the (blocking) single-file copy a run-loop turn so the bar paints.
        DispatchQueue.main.async {
            let r = DeviceService.copy(ctx: ctx, device: device, srcPaths: [path])
            self.copyNextFile(
                device, paths: paths, index: index + 1,
                copied: copied + (r?.copied ?? 0),
                skipped: skipped + (r?.skipped ?? 0))
        }
    }

    /// Two-way sync the device. Auto (single-side) changes apply immediately;
    /// both-changed conflicts are skipped for now (the resolution sheet is the
    /// next phase) and reported in the status line.
    func syncDevice(_ device: Device) {
        guard let ctx = ctx, !deviceBusy else { return }
        deviceBusy = true
        deviceStatus = nil
        DispatchQueue.main.async {
            guard let plan = DeviceService.syncPlan(ctx: ctx, device: device) else {
                self.deviceBusy = false
                self.deviceStatus = "Sync failed"
                return
            }
            // Apply auto pairs with no conflict choices (conflicts are skipped
            // inside the core until the conflict sheet lands).
            let result = DeviceService.applySync(
                ctx: ctx, device: device, plan: plan, choices: [])
            self.loadDeviceTracks(device)
            self.refreshDeviceCounts(for: device)
            self.deviceBusy = false
            let applied = result?.applied ?? 0
            let conflicts = plan.conflicts.count
            if conflicts > 0 {
                self.deviceStatus =
                    "Synced \(applied) · \(conflicts) conflict\(conflicts == 1 ? "" : "s") need resolving (coming soon)"
            } else {
                self.deviceStatus = "Synced \(applied) change\(applied == 1 ? "" : "s")"
            }
        }
    }

    /// Re-read the device's files from disk.
    func scanDevice(_ device: Device) {
        guard !deviceBusy else { return }
        deviceBusy = true
        deviceStatus = nil
        DispatchQueue.main.async {
            self.loadDeviceTracks(device)
            self.refreshDeviceCounts(for: device)
            self.deviceBusy = false
            self.deviceStatus = "Scanned \(self.deviceTracks.count) file\(self.deviceTracks.count == 1 ? "" : "s")"
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
