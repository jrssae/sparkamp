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
}
