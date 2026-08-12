import SwiftUI
import AppKit

// Split out of SettingsWindow.swift on 2026-08-12: the file was 1,087
// lines of five unrelated panes. They were already siblings of
// `SettingsView` rather than nested in it, so each moved whole — the
// only edit is `private struct` -> `struct`, because file-private would
// now hide the pane from the host that instantiates it.

// MARK: - Appearance pane

struct AppearancePane: View {
    @EnvironmentObject var themeManager: ThemeManager

    @State private var entries: [ThemeManager.SkinEntry] = []
    @State private var selection: String? = nil
    @State private var errorMessage: String? = nil

    var body: some View {
        Form {
            Section("Skin") {
                List(entries, selection: $selection) { entry in
                    HStack {
                        Text(entry.displayName)
                        if entry.isBuiltin {
                            Text("(built-in)")
                                .foregroundStyle(.secondary)
                        }
                        Spacer()
                        if entry.name == themeManager.activeSkin {
                            Image(systemName: "checkmark.circle.fill")
                                .foregroundStyle(.tint)
                        }
                    }
                    .tag(entry.name)
                }
                .frame(minHeight: 180)
                .onChange(of: selection) { _, new in
                    if let new, new != themeManager.activeSkin {
                        themeManager.setActiveSkin(new)
                    }
                }

                HStack {
                    Button("Add skin…")     { addSkin() }
                    Button("Remove")        { removeSelected() }
                        .disabled(isBuiltinSelected)
                    Button("Download skin…") { downloadSelected() }
                        .disabled(selection == nil)
                }
            }

            Section("Documentation") {
                Button("Export how-to guide…") { exportGuide() }
                Text("A markdown reference describing every variable in the skin format and which UI elements it controls.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
        .alert("Could not add skin",
               isPresented: Binding(
                   get: { errorMessage != nil },
                   set: { if !$0 { errorMessage = nil } })) {
            Button("OK") { errorMessage = nil }
        } message: {
            Text(errorMessage ?? "")
        }
        .onAppear {
            entries = themeManager.listSkins()
            selection = themeManager.activeSkin
        }
    }

    // MARK: Actions

    private var isBuiltinSelected: Bool {
        guard let s = selection else { return true }
        return s == "dark" || s == "light"
    }

    private func addSkin() {
        let panel = NSOpenPanel()
        panel.title = "Add Sparkamp skin"
        panel.allowedContentTypes = [.init(filenameExtension: "css")!]
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.begin { response in
            guard response == .OK, let url = panel.url else { return }
            Task { @MainActor in
                switch themeManager.addUserSkin(from: url) {
                case .success(let entry):
                    entries = themeManager.listSkins()
                    themeManager.setActiveSkin(entry.name)
                    selection = entry.name
                case .failure(let err):
                    switch err {
                    case .unreadable:
                        errorMessage = "The selected file could not be read."
                    case .noRootBlock:
                        errorMessage = "The file is not a valid Sparkamp skin — missing a :root { } block."
                    case .copyFailed:
                        errorMessage = "Could not copy the skin into the user skins directory."
                    }
                }
            }
        }
    }

    private func removeSelected() {
        guard let s = selection, !isBuiltinSelected else { return }
        themeManager.hideSkin(s)
        entries = themeManager.listSkins()
        selection = themeManager.activeSkin
    }

    private func downloadSelected() {
        guard let s = selection else { return }
        let panel = NSSavePanel()
        panel.title = "Save Sparkamp skin"
        panel.nameFieldStringValue = "\(s).css"
        panel.allowedContentTypes = [.init(filenameExtension: "css")!]
        panel.begin { response in
            guard response == .OK, let url = panel.url else { return }
            Task { @MainActor in
                themeManager.exportSkin(s, to: url)
            }
        }
    }

    private func exportGuide() {
        let panel = NSSavePanel()
        panel.title = "Save Sparkamp skin guide"
        panel.nameFieldStringValue = "sparkamp-skin-guide.md"
        panel.allowedContentTypes = [.init(filenameExtension: "md")!]
        panel.begin { response in
            guard response == .OK, let url = panel.url else { return }
            Task { @MainActor in
                themeManager.exportGuide(to: url)
            }
        }
    }
}
