import Foundation
import ImageCaptureCore

// MARK: - iOS / PTP "unsupported" device recognition
//
// An iPhone / iPad — or any camera or Android phone in PTP photo mode — is
// never a music-sync target (iOS has no filesystem-reachable music store; PTP
// exposes only a camera roll). These devices never mount under /Volumes, so the
// volume enumeration in DeviceService misses them entirely. ImageCaptureCore's
// ICDeviceBrowser sees them instead.
//
// Each such device is surfaced as a synthetic `Device` with backend
// `.unsupported` and `fsVisible == false`, so the existing sidebar/overview/
// detail UI renders it with an honest "can't sync music" banner and no sync or
// copy path — the macOS equivalent of the GTK gphoto2/afc recognition.

/// Apple's USB vendor ID — an iPhone/iPad/iPod reports this.
private let appleUSBVendorID = 0x05AC

final class UnsupportedDeviceWatcher: NSObject, ICDeviceBrowserDelegate {

    /// Called on the main thread whenever the set of unsupported devices
    /// changes, with the full current list.
    var onChange: (([Device]) -> Void)?

    private let browser = ICDeviceBrowser()
    /// Current devices keyed by the ICDevice identity so add/remove stay in sync.
    private var seen: [ObjectIdentifier: Device] = [:]

    override init() {
        super.init()
        browser.delegate = self
        // Browse local cameras (this class covers PTP/iOS). The mask needs both
        // a device-type bit and a location bit or nothing is reported.
        browser.browsedDeviceTypeMask = ICDeviceTypeMask(rawValue:
            ICDeviceTypeMask.camera.rawValue | ICDeviceLocationTypeMask.local.rawValue)!
    }

    /// Start browsing (idempotent). Callbacks arrive on the calling run loop, so
    /// start from the main thread to publish safely into the model.
    func start() {
        guard !browser.isBrowsing else { return }
        browser.start()
    }

    func stop() {
        guard browser.isBrowsing else { return }
        browser.stop()
    }

    // MARK: ICDeviceBrowserDelegate

    func deviceBrowser(_ browser: ICDeviceBrowser, didAdd device: ICDevice,
                       moreComing: Bool) {
        seen[ObjectIdentifier(device)] = Self.synthetic(from: device)
        if !moreComing { publish() }
    }

    func deviceBrowser(_ browser: ICDeviceBrowser, didRemove device: ICDevice,
                       moreGoing: Bool) {
        seen[ObjectIdentifier(device)] = nil
        if !moreGoing { publish() }
    }

    private func publish() {
        let list = seen.values.sorted { $0.label.localizedCaseInsensitiveCompare($1.label) == .orderedAscending }
        onChange?(list)
    }

    // MARK: Mapping

    /// Build a synthetic `Device` for an ImageCaptureCore device. `fsType`
    /// carries "ios"/"ptp" so the detail view can pick the right banner text.
    private static func synthetic(from device: ICDevice) -> Device {
        let name = device.name ?? "Device"
        let isApple = device.usbVendorID == appleUSBVendorID
            || name.localizedCaseInsensitiveContains("iphone")
            || name.localizedCaseInsensitiveContains("ipad")
            || name.localizedCaseInsensitiveContains("ipod")
        let id = device.uuidString ?? name
        return Device(
            id: id,
            label: name,
            mountPath: "",
            fsType: isApple ? "ios" : "ptp",
            totalBytes: 0,
            freeBytes: 0,
            readOnly: true,
            ejectable: false,
            backendId: id,
            backend: .unsupported,
            fsVisible: false)
    }
}
