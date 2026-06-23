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
