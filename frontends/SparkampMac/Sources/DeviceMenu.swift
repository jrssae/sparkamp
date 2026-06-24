import AppKit

// Shared "Send to Playlist" / "Send to Device" context-menu submenus, so the
// active playlist, the Media Library files view, and the saved-playlist editor
// all offer the same actions over a set of file paths.

extension SparkampModel {

    /// "Send to Playlist ▸ (New Playlist… + each saved playlist)" — appends the
    /// given file paths to the chosen playlist, or seeds a new one.
    func sendToPlaylistMenuItem(paths: [String]) -> NSMenuItem {
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
        let parent = NSMenuItem(title: "Send to Playlist", action: nil, keyEquivalent: "")
        parent.submenu = sub
        parent.isEnabled = !paths.isEmpty
        return parent
    }

    /// "Send to Device ▸ (each writable connected device)" — copies the given
    /// file paths onto the chosen device under Music/<file>.
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
}
