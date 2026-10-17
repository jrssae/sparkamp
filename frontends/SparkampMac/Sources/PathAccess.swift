import AppKit

// Access to songs outside the library under the App Sandbox.
//
// The App Store build has no entitlement for the Music folder. It can open
// what the user picks or drops, and what a stored bookmark gives back:
// library folders, granted volumes, and the grants in the core's
// path_grants table. Songs added by a pick or a drop remember their own
// grant (sparkamp_playlist_add / _add_fast). What is left is a list of paths
// that arrives some other way, a saved playlist or an internal drag, naming
// songs nobody has granted yet. Those need the user to grant their folder,
// and that is this prompt.
//
// It also suggests adding the folder to the Library, with a button that
// does it, because a library folder is the grant that never needs asking
// again.

extension SparkampModel {

    /// The user's real Music folder.
    ///
    /// Not `FileManager`'s answer: under the App Sandbox the home directory
    /// it reports is the app's container. The password database still names
    /// the real one, which is the folder an open panel should start in.
    static var musicFolder: URL {
        let home = getpwuid(getuid()).flatMap { $0.pointee.pw_dir }.map { String(cString: $0) }
            ?? NSHomeDirectory()
        return URL(fileURLWithPath: home, isDirectory: true)
            .appendingPathComponent("Music", isDirectory: true)
    }

    /// Before `paths` go into the playlist, ask for the folder they are in if
    /// Sparkamp cannot open them yet. Does nothing when they all open.
    ///
    /// Modal, so a caller that needs the new rows straight afterwards (see
    /// `addFiles`) still gets them. Songs the user declines to grant are left
    /// to the add, which skips what it cannot open, as it always has.
    func ensureAccess(toPlay paths: [String]) {
        guard let folder = Self.folderNeedingAccess(paths) else { return }
        guard askForAccess(to: folder) else { return }
        if Self.folderNeedingAccess(paths) != nil {
            let alert = NSAlert()
            alert.messageText = "Some songs still can't be opened"
            alert.informativeText = """
                The folder you chose doesn't contain all of them. Sparkamp \
                will add the songs it can open.
                """
            alert.runModal()
        }
    }

    /// The folder to ask for before `paths` can play, or nil when every one
    /// of them opens. Answered by the core, which opens each file.
    static func folderNeedingAccess(_ paths: [String]) -> String? {
        guard !paths.isEmpty else { return nil }
        let nsStrings: [NSString] = paths.map { $0 as NSString }
        let cPaths: [UnsafePointer<CChar>?] = nsStrings.map { $0.utf8String }
        let raw = cPaths.withUnsafeBufferPointer { buf in
            sparkamp_folder_needing_access(buf.baseAddress, Int32(paths.count))
        }
        guard let raw else { return nil }
        defer { sparkamp_free_string(raw) }
        return String(cString: raw)
    }

    /// Ask for `folder`: allow access to it, or add it to the Library.
    /// Returns whether the user granted it either way.
    private func askForAccess(to folder: String) -> Bool {
        let name = FileManager.default.displayName(atPath: folder)
        let alert = NSAlert()
        alert.messageText = "Allow Sparkamp to play songs in \u{201C}\(name)\u{201D}?"
        alert.informativeText = """
            macOS needs your permission before Sparkamp can open these songs. \
            You only have to do this once for this folder.

            Tip: add \u{201C}\(name)\u{201D} to your Library and Sparkamp can \
            always play everything in it, without asking.
            """
        alert.addButton(withTitle: "Allow Access\u{2026}")
        alert.addButton(withTitle: "Add \u{201C}\(name)\u{201D} to Library\u{2026}")
        alert.addButton(withTitle: "Cancel")

        switch alert.runModal() {
        case .alertFirstButtonReturn:
            guard let url = pickFolder(folder, prompt: "Allow Access") else { return false }
            if let ctx {
                _ = url.path.withCString { sparkamp_grant_access(ctx, $0) }
            }
            return true
        case .alertSecondButtonReturn:
            guard let url = pickFolder(folder, prompt: "Add to Library") else { return false }
            // The library keeps its own bookmark for every folder it holds,
            // taken inside the add while this pick is live.
            mlEnsureOpen()
            mlAddFolder(url.path)
            return true
        default:
            return false
        }
    }

    /// An open panel already showing `folder`, so granting it is one click.
    /// The sandbox only grants what the user picks in a panel; no other kind
    /// of "Allow" button can give access to a folder.
    private func pickFolder(_ folder: String, prompt: String) -> URL? {
        let panel = NSOpenPanel()
        let name = FileManager.default.displayName(atPath: folder)
        panel.message = "Choose \(prompt) to give Sparkamp access to \u{201C}\(name)\u{201D}."
        panel.prompt = prompt
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.directoryURL = URL(fileURLWithPath: folder, isDirectory: true)
        return panel.runModal() == .OK ? panel.url : nil
    }
}
