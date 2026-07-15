import AppKit
import SwiftUI

// Shared "Send to" building blocks, so the active playlist, the Media
// Library files view, the saved-playlist editor, and a connected device's
// file list all offer the same actions over a set of file paths.
//
// `SendEntry` + `sendToSpec` mirror the GTK reference 1:1
// (frontends/gtk/window/util.rs `SendEntry` / `send_to_spec` /
// `build_send_to_menu`) — pure data, no AppKit/SwiftUI, so the 0/1/N
// visibility rule (zero of a kind ⇒ no entry, one ⇒ a direct item, two+ ⇒ a
// submenu) is defined exactly once and shared by both the NSMenu builder
// below (`sendToMenuItems`, used by the NSTableView-backed context menus in
// MLFilesTable.swift / MLPlaylistEditor.swift) and the SwiftUI `SendToMenu`
// view at the bottom (used by DeviceDetailView.swift's native
// `.contextMenu(forSelectionType:)` and the Files-view toolbar button).

/// One drive/device (id, label) pair for the Send-to spec. A concrete
/// struct rather than a bare tuple — SwiftUI's `ForEach(_:id:)` key paths
/// can't address tuple elements (`\.id` on `(id: String, label: String)`
/// fails to compile), and `Identifiable` conformance lets `ForEach` pick it
/// up with no explicit `id:` argument at every call site below.
struct SendTarget: Identifiable, Hashable {
    let id: String
    let label: String
}

/// What the "Send to" menu shows, as data. See file header.
enum SendEntry {
    case activePlaylist
    case savedPlaylist
    /// Exactly one drive/device attached: a direct item, no submenu.
    case driveDirect(SendTarget)
    /// Two or more: a submenu, one item per target.
    case driveMenu([SendTarget])
    case deviceDirect(SendTarget)
    case deviceMenu([SendTarget])
}

/// Mirrors GTK's `send_to_spec`. Always includes `.activePlaylist` and
/// `.savedPlaylist` — it's the caller's job (via `includeActive`, below)
/// to skip rendering Active Playlist for a view where it wouldn't make
/// sense (the active playlist's own row context menu).
func sendToSpec(drives: [SendTarget], devices: [SendTarget]) -> [SendEntry] {
    var out: [SendEntry] = [.activePlaylist, .savedPlaylist]
    switch drives.count {
    case 0: break
    case 1: out.append(.driveDirect(drives[0]))
    default: out.append(.driveMenu(drives))
    }
    switch devices.count {
    case 0: break
    case 1: out.append(.deviceDirect(devices[0]))
    default: out.append(.deviceMenu(devices))
    }
    return out
}

extension SparkampModel {

    /// "Saved Playlist ▸ (New Playlist… + each saved playlist)" — appends the
    /// given file paths to the chosen playlist, or seeds a new one. `title`
    /// defaults to the item's pre-SendToMenu wording ("Send to Playlist") so
    /// the one remaining direct caller (PlaylistView.swift's active-playlist
    /// context menu, not in this task's scope) keeps its existing label;
    /// `sendToMenuItems` below passes "Saved Playlist" to match the GTK spec.
    func sendToPlaylistMenuItem(paths: [String], title: String = "Send to Playlist") -> NSMenuItem {
        let sub = NSMenu()
        sub.autoenablesItems = false
        sub.addItem(BlockMenuItem(title: "New Playlist…", enabled: !paths.isEmpty) {
            self.createPlaylistFromPaths(paths)
        })
        if !mlSavedPlaylists.isEmpty {
            sub.addItem(.separator())
            for pl in mlSavedPlaylists {
                let pid = pl.id
                sub.addItem(BlockMenuItem(title: pl.name, enabled: !paths.isEmpty) {
                    self.mlAppendPathsToPlaylist(playlistId: pid, paths: paths)
                })
            }
        }
        let parent = NSMenuItem(title: title, action: nil, keyEquivalent: "")
        parent.submenu = sub
        parent.isEnabled = !paths.isEmpty
        return parent
    }

    /// "Send to Device ▸ (each writable connected device)" — copies the given
    /// file paths onto the chosen device under Music/<file>. Kept as-is for
    /// PlaylistView.swift; `sendToMenuItems` below applies the 0/1/N rule
    /// instead of always nesting a submenu.
    func sendToDeviceMenuItem(paths: [String]) -> NSMenuItem {
        let sub = NSMenu()
        sub.autoenablesItems = false
        let writable = devices.filter { $0.fsVisible && !$0.readOnly }
        if writable.isEmpty {
            sub.addItem(BlockMenuItem(title: "No devices connected", enabled: false) {})
        } else {
            for dev in writable {
                let d = dev
                let name = d.label.isEmpty ? "Untitled" : d.label
                sub.addItem(BlockMenuItem(title: name, enabled: !paths.isEmpty) {
                    self.copyToDevice(d, paths: paths)
                })
            }
        }
        let parent = NSMenuItem(title: "Send to Device", action: nil, keyEquivalent: "")
        parent.submenu = sub
        parent.isEnabled = !paths.isEmpty && !writable.isEmpty
        return parent
    }

    /// Run the Save panel and create a saved playlist seeded with `paths`.
    func createPlaylistFromPaths(_ paths: [String]) {
        guard !paths.isEmpty else { return }
        runPlaylistSavePanel(model: self,
                             defaultName: defaultTimestampedPlaylistName()) { stem, dir in
            _ = self.mlSavePlaylistAs(name: stem, trackPaths: paths, directory: dir)
            self.mlRefreshSavedPlaylists()
        }
    }

    /// Copy `paths` onto the writable device with this id — a no-op if the
    /// device vanished between menu build and click.
    func sendPathsToDevice(_ id: String, paths: [String]) {
        guard !paths.isEmpty, let dev = devices.first(where: { $0.id == id }) else { return }
        copyToDevice(dev, paths: paths)
    }

    /// Writable devices as Send-to targets — the same `fsVisible &&
    /// !readOnly` filter `sendToDeviceMenuItem` has always used.
    var writableSendToDevices: [SendTarget] {
        devices.filter { $0.fsVisible && !$0.readOnly }
            .map { SendTarget(id: $0.id, label: $0.label) }
    }

    /// Every optical drive as Send-to targets.
    var sendToDriveTargets: [SendTarget] {
        discDrives.map { SendTarget(id: $0.id, label: $0.label) }
    }

    /// Build the unified "Send to" menu as a flat NSMenuItem list, in GTK's
    /// send_to_spec order: Active Playlist (only when `includeActive`) /
    /// Saved Playlist ▸ / Disc Drive [direct when exactly one, ▸ otherwise] /
    /// Removable Device [same rule]. Wrap the result under one "Send to"
    /// parent item (see `sendToMenuItem`) for a context menu, or set it
    /// directly as a MenuButton's items for a "Send to ▾" toolbar button.
    func sendToMenuItems(paths: [String], includeActive: Bool) -> [NSMenuItem] {
        var items: [NSMenuItem] = []
        for entry in sendToSpec(drives: sendToDriveTargets, devices: writableSendToDevices) {
            switch entry {
            case .activePlaylist:
                guard includeActive else { continue }
                items.append(BlockMenuItem(title: "Active Playlist", enabled: !paths.isEmpty) {
                    self.addFiles(paths.map { URL(fileURLWithPath: $0) })
                })
            case .savedPlaylist:
                items.append(sendToPlaylistMenuItem(paths: paths, title: "Saved Playlist"))
            case .driveDirect(let d):
                items.append(BlockMenuItem(title: "Disc Drive", enabled: !paths.isEmpty) {
                    self.sendPathsToDrive(d.id, paths: paths)
                })
            case .driveMenu(let list):
                let sub = NSMenu()
                sub.autoenablesItems = false
                for d in list {
                    sub.addItem(BlockMenuItem(title: d.label, enabled: !paths.isEmpty) {
                        self.sendPathsToDrive(d.id, paths: paths)
                    })
                }
                let parent = NSMenuItem(title: "Disc Drive", action: nil, keyEquivalent: "")
                parent.submenu = sub
                parent.isEnabled = !paths.isEmpty
                items.append(parent)
            case .deviceDirect(let d):
                items.append(BlockMenuItem(title: "Removable Device", enabled: !paths.isEmpty) {
                    self.sendPathsToDevice(d.id, paths: paths)
                })
            case .deviceMenu(let list):
                let sub = NSMenu()
                sub.autoenablesItems = false
                for d in list {
                    let name = d.label.isEmpty ? "Untitled" : d.label
                    sub.addItem(BlockMenuItem(title: name, enabled: !paths.isEmpty) {
                        self.sendPathsToDevice(d.id, paths: paths)
                    })
                }
                let parent = NSMenuItem(title: "Removable Device", action: nil, keyEquivalent: "")
                parent.submenu = sub
                parent.isEnabled = !paths.isEmpty
                items.append(parent)
            }
        }
        return items
    }

    /// The full "Send to" menu as one NSMenuItem with a submenu — what every
    /// NSMenu-based context menu wants (mirrors GTK's
    /// `menu.append_submenu(Some("Send to"), &send)`).
    func sendToMenuItem(paths: [String], includeActive: Bool) -> NSMenuItem {
        let sub = NSMenu()
        sub.autoenablesItems = false
        for item in sendToMenuItems(paths: paths, includeActive: includeActive) {
            sub.addItem(item)
        }
        let parent = NSMenuItem(title: "Send to", action: nil, keyEquivalent: "")
        parent.submenu = sub
        parent.isEnabled = !paths.isEmpty
        return parent
    }
}

// MARK: - SwiftUI "Send to" content (native context menus / menu buttons)

/// SwiftUI counterpart of `sendToMenuItems` above — same `sendToSpec`, same
/// 0/1/N rule, same wording — for the views that use SwiftUI's native
/// `.contextMenu(forSelectionType:)` or a `Menu` button instead of an
/// AppKit NSMenu (DeviceDetailView.swift; the Files-view "Send to ▾"
/// toolbar button in MediaLibraryWindow.swift). Embed as
/// `Menu("Send to") { SendToMenu(paths: paths) }` for a context-menu
/// submenu, or directly inside a top-level `Menu("Send to ▾") { … }` for a
/// toolbar button — both share this content.
struct SendToMenu: View {
    @EnvironmentObject var model: SparkampModel
    let paths: [String]
    var includeActive: Bool = true

    var body: some View {
        // Index-based, not `.enumerated()` — `ForEach(_:id:)` key paths
        // can't address tuple elements, and `.enumerated()` yields tuples.
        let entries = sendToSpec(drives: model.sendToDriveTargets,
                                  devices: model.writableSendToDevices)
        ForEach(0..<entries.count, id: \.self) { i in
            entryView(entries[i])
        }
    }

    @ViewBuilder
    private func entryView(_ entry: SendEntry) -> some View {
        switch entry {
        case .activePlaylist:
            if includeActive {
                Button("Active Playlist") {
                    model.addFiles(paths.map { URL(fileURLWithPath: $0) })
                }
                .disabled(paths.isEmpty)
            }
        case .savedPlaylist:
            Menu("Saved Playlist") {
                Button("New Playlist…") { model.createPlaylistFromPaths(paths) }
                    .disabled(paths.isEmpty)
                if !model.mlSavedPlaylists.isEmpty {
                    Divider()
                    ForEach(model.mlSavedPlaylists) { pl in
                        Button(pl.name) {
                            model.mlAppendPathsToPlaylist(playlistId: pl.id, paths: paths)
                        }
                        .disabled(paths.isEmpty)
                    }
                }
            }
        case .driveDirect(let d):
            Button("Disc Drive") { model.sendPathsToDrive(d.id, paths: paths) }
                .disabled(paths.isEmpty)
        case .driveMenu(let list):
            Menu("Disc Drive") {
                ForEach(list) { d in
                    Button(d.label) { model.sendPathsToDrive(d.id, paths: paths) }
                        .disabled(paths.isEmpty)
                }
            }
        case .deviceDirect(let d):
            Button("Removable Device") { model.sendPathsToDevice(d.id, paths: paths) }
                .disabled(paths.isEmpty)
        case .deviceMenu(let list):
            Menu("Removable Device") {
                ForEach(list) { d in
                    Button(d.label.isEmpty ? "Untitled" : d.label) {
                        model.sendPathsToDevice(d.id, paths: paths)
                    }
                    .disabled(paths.isEmpty)
                }
            }
        }
    }
}
