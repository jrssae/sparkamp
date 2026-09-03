import SwiftUI
import AppKit

// Split out of SettingsWindow.swift on 2026-08-12: the file was 1,087
// lines of five unrelated panes. They were already siblings of
// `SettingsView` rather than nested in it, so each moved whole — the
// only edit is `private struct` -> `struct`, because file-private would
// now hide the pane from the host that instantiates it.

// MARK: - Media Library pane

struct MediaLibraryPane: View {
    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager

    /// Analyze ReplayGain for newly added/scanned files automatically.
    @State private var rgAutoAnalyze: Bool = false
    /// Write computed ReplayGain values back into MP3 tags (non-MP3 skipped).
    @State private var rgWriteTags: Bool = false

    // Watch folders (Phase 8 Task 9 FFI, wired here in Task 12). Defaults
    // mirror MediaLibraryConfig's Default impl (src/config.rs).
    @State private var watchFolders: Bool = true
    @State private var autoAddPlayed: Bool = false
    @State private var removeMissingOnRescan: Bool = false
    @State private var compactOnRescan: Bool = false
    @State private var rescanOnStartup: Bool = false
    /// Per-folder recurse flags, keyed by folder path, mirrored from the DB
    /// on appear and whenever the folder list changes.
    ///
    /// A binding that read `sparkamp_ml_folder_recurse` straight through on
    /// every render looked simpler, but it gave the checkbox no state
    /// SwiftUI could observe: it redrew correctly only because the model's
    /// 10 Hz tick happens to invalidate this pane, and it cost two SQLite
    /// queries per folder per redraw on the main thread. GTK reads the flag
    /// once as it builds each row too (`frontends/gtk/window/settings.rs`).
    @State private var folderRecurse: [String: Bool] = [:]
    /// F12.1: restore each Media-Library view's search query on next open.
    @State private var rememberSearch: Bool = false
    /// F12.2: display/group a track with no album-artist tag under its
    /// artist instead of blank.
    @State private var artistAsAlbumArtist: Bool = false
    /// F12.3: skip opening the Media-Library database at startup; it opens
    /// lazily on first demand (ML window, device sync, or the folder
    /// watcher's first need — see `openMediaLibrary()`). Inert on mac today:
    /// unlike GTK, mac has never eagerly opened the DB at startup (Phase 8
    /// baseline — `sparkamp_ml_open` is only ever called from demand sites),
    /// so this toggle mainly keeps the persisted config in sync across
    /// platforms rather than changing mac's own runtime behaviour.
    @State private var skipDbLoad: Bool = false

    var body: some View {
        let vars = themeManager.currentVars
        return Form {
            // ── Rescan ─────────────────────────────────────────────────────
            // Rescan and Cancel Scan swap places, the way GTK's pair does —
            // a running scan had no way to be stopped from here before.
            Section("Scan") {
                HStack {
                    if model.mlScanRunning {
                        Button("Cancel Scan") { model.mlCancelScan() }
                            .buttonStyle(.bordered)
                    } else {
                        Button("Rescan") {
                            model.openMediaLibrary()
                            model.mlRescanAll()
                        }
                        .buttonStyle(.borderedProminent)
                    }
                }
                if model.mlScanRunning {
                    ProgressView(
                        value: model.mlScanTotal > 0
                            ? Double(model.mlScanDone) / Double(model.mlScanTotal) : 0
                    )
                    Text(model.mlScanTotal > 0
                         ? "Scanning \(model.mlScanDone)/\(model.mlScanTotal)…"
                         : "Scanning…")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }

            // ── ReplayGain analysis ────────────────────────────────────────
            Section("ReplayGain") {
                HStack {
                    if model.rgRunning {
                        Button("Cancel Analysis") { model.rgCancelAnalyze() }
                            .buttonStyle(.bordered)
                    } else {
                        Button("Analyze ReplayGain") {
                            model.openMediaLibrary()
                            model.rgAnalyzeMissing()
                        }
                        .buttonStyle(.borderedProminent)

                        Button("Force Recalculate") {
                            model.openMediaLibrary()
                            let ids = model.mlAllTracks().map(\.id)
                            model.rgAnalyzeSelection(ids: ids)
                        }
                        .buttonStyle(.bordered)
                    }
                }
                if model.rgRunning {
                    ProgressView(
                        value: model.rgTotal > 0
                            ? Double(model.rgDone) / Double(model.rgTotal) : 0
                    )
                    Text(model.rgTotal > 0
                         ? "Analyzing \(model.rgDone)/\(model.rgTotal)…"
                         : "Analyzing ReplayGain…")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Toggle("Analyze ReplayGain on add/scan", isOn: $rgAutoAnalyze)
                    .onChange(of: rgAutoAnalyze) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_rg_auto_analyze(ctx, newValue)
                        sparkamp_save_config(ctx)
                    }
                Toggle("Write ReplayGain tags to files", isOn: $rgWriteTags)
                    .onChange(of: rgWriteTags) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_rg_write_tags(ctx, newValue)
                        sparkamp_save_config(ctx)
                    }
            }

            // ── Folder watching (Phase 8 Task 12) ──────────────────────────
            Section("Folder Watching") {
                Toggle("Watch folders for changes", isOn: $watchFolders)
                    .onChange(of: watchFolders) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        // The Rust setter already (re)builds the watcher —
                        // no extra call needed here.
                        sparkamp_set_watch_folders(ctx, newValue)
                        sparkamp_save_config(ctx)
                    }
                Toggle("Automatically add played tracks", isOn: $autoAddPlayed)
                    .onChange(of: autoAddPlayed) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_auto_add_played(ctx, newValue)
                        sparkamp_save_config(ctx)
                    }
                Toggle("Remove missing files on rescan", isOn: $removeMissingOnRescan)
                    .onChange(of: removeMissingOnRescan) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_remove_missing_on_rescan(ctx, newValue)
                        sparkamp_save_config(ctx)
                    }
                Toggle("Compact database after rescan", isOn: $compactOnRescan)
                    .onChange(of: compactOnRescan) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_compact_on_rescan(ctx, newValue)
                        sparkamp_save_config(ctx)
                    }
                Toggle("Rescan all folders on startup", isOn: $rescanOnStartup)
                    .onChange(of: rescanOnStartup) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_rescan_on_startup(ctx, newValue)
                        sparkamp_save_config(ctx)
                    }
            }

            // ── Library behavior (F12.1–F12.3) ─────────────────────────────
            // Their own section: these three have nothing to do with folder
            // watching, and reading as if they did is misleading.
            Section("Library Behavior") {
                Toggle("Remember search per view", isOn: $rememberSearch)
                    .onChange(of: rememberSearch) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_remember_search(ctx, newValue)
                        sparkamp_save_config(ctx)
                    }
                Toggle("Treat artist as album artist", isOn: $artistAsAlbumArtist)
                    .onChange(of: artistAsAlbumArtist) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_artist_as_album_artist(ctx, newValue)
                        sparkamp_save_config(ctx)
                    }
                Toggle("Skip database load at startup", isOn: $skipDbLoad)
                    .onChange(of: skipDbLoad) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_skip_db_load(ctx, newValue)
                        sparkamp_save_config(ctx)
                    }
            }

            // ── Watched folders ────────────────────────────────────────────
            Section {
                if model.mlFolders.isEmpty {
                    Text("No folders added yet.")
                        .foregroundStyle(.secondary)
                        .font(vars.bodyFont)
                } else {
                    ForEach(model.mlFolders, id: \.self) { folder in
                        HStack {
                            Image(systemName: "folder")
                                .foregroundStyle(.secondary)
                            Text(folder)
                                .font(vars.bodyFont)
                                .lineLimit(1)
                                .truncationMode(.middle)
                            Spacer()
                            // Per-folder recurse (Phase 8 Task 12). Reads the
                            // mirror; writes through to the DB and rebuilds
                            // the watcher so the new mode takes effect on the
                            // live watch, not just the next scan.
                            Toggle("Recurse", isOn: Binding(
                                get: { folderRecurse[folder] ?? true },
                                set: { newValue in
                                    folderRecurse[folder] = newValue
                                    guard let ctx = model.ctx else { return }
                                    folder.withCString {
                                        sparkamp_ml_set_folder_recurse(ctx, $0, newValue)
                                    }
                                    sparkamp_ml_watch_rebuild(ctx)
                                }
                            ))
                            .toggleStyle(.checkbox)
                            .font(vars.bodyFont)
                            .help("Include subfolders when scanning/watching this folder")
                            Button {
                                model.mlRemoveFolder(folder)
                            } label: {
                                Image(systemName: "minus.circle")
                                    .foregroundStyle(.red)
                            }
                            .buttonStyle(.plain)
                        }
                    }
                }
            } header: {
                HStack {
                    Text("Watched Folders")
                    Spacer()
                    Button {
                        model.openMediaLibrary()
                        model.mlOpenAddFolderPicker()
                    } label: {
                        Label("Add Folder…", systemImage: "plus")
                            .font(vars.bodyFont)
                    }
                    .buttonStyle(.borderless)
                }
            }

            // Disc and gnudb settings live in the Behavior tab, where GTK
            // keeps them.

            // ── Tools ──────────────────────────────────────────────────────
            Section("Tools") {
                HStack {
                    VStack(alignment: .leading, spacing: 4) {
                        Text("Find Duplicates")
                            .font(vars.bodyFont.weight(.medium))
                        Text("Scan your media library for duplicate tracks using title, artist, and duration matching.")
                            .font(vars.bodyFont)
                            .foregroundStyle(.secondary)
                    }
                    Spacer()
                    Button("Scan…") {
                        model.dedupVisible = true
                    }
                    .buttonStyle(.bordered)
                }
                .padding(.vertical, 2)
            }
        }
        .formStyle(.grouped)
        .onAppear {
            // Ensure folder list is fresh when the pane is shown.
            if model.mlIsOpen { model.mlRefreshFolders() }
            if let ctx = model.ctx {
                rgAutoAnalyze = sparkamp_get_rg_auto_analyze(ctx)
                rgWriteTags = sparkamp_get_rg_write_tags(ctx)
                watchFolders = sparkamp_get_watch_folders(ctx)
                autoAddPlayed = sparkamp_get_auto_add_played(ctx)
                removeMissingOnRescan = sparkamp_get_remove_missing_on_rescan(ctx)
                compactOnRescan = sparkamp_get_compact_on_rescan(ctx)
                rescanOnStartup = sparkamp_get_rescan_on_startup(ctx)
                rememberSearch = sparkamp_get_remember_search(ctx)
                artistAsAlbumArtist = sparkamp_get_artist_as_album_artist(ctx)
                skipDbLoad = sparkamp_get_skip_db_load(ctx)
            }
            loadFolderRecurse()
        }
        .onChange(of: model.mlFolders) { _, _ in loadFolderRecurse() }
    }

    /// Re-read every watched folder's recurse flag into `folderRecurse`.
    /// Called on appear and whenever the folder list changes — the only two
    /// moments the flags can go stale, since nothing outside this pane
    /// writes them.
    private func loadFolderRecurse() {
        guard let ctx = model.ctx else { return }
        var flags: [String: Bool] = [:]
        for folder in model.mlFolders {
            flags[folder] = folder.withCString { sparkamp_ml_folder_recurse(ctx, $0) }
        }
        folderRecurse = flags
    }
}
