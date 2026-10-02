import SwiftUI
import AppKit
import UniformTypeIdentifiers

// MARK: - Playlist management (nav = .playlists)

struct MLPlaylistManagement: View {
    @Binding var nav: MLNavigation
    /// Live text from the toolbar's search box, in the same slot as the Files
    /// and Albums ones. Matched against the name, and the server's name for
    /// a server playlist.
    let searchQuery: String
    let theme: SkinTheme

    @EnvironmentObject var model: SparkampModel

    @State private var showingRename = false
    @State private var renameText    = ""
    @State private var renameTarget: Int64? = nil

    private var visiblePlaylists: [MLPlaylistItem] {
        let q = searchQuery.trimmingCharacters(in: .whitespaces)
        guard !q.isEmpty else { return model.mlSavedPlaylists }
        return model.mlSavedPlaylists.filter {
            $0.name.localizedCaseInsensitiveContains(q)
                || ($0.serverName?.localizedCaseInsensitiveContains(q) ?? false)
        }
    }

    var body: some View {
        VStack(spacing: 0) {
            // Header
            HStack {
                Text("Playlists")
                    .font(theme.vars.bodyFont.weight(.semibold))
                    .foregroundStyle(theme.playlistDurationText)
                Spacer()
                // Prominent New Playlist control — uses the same native
                // Save panel as the active-playlist Save button and the
                // right-click "New Playlist…" entry.  Single consistent
                // path for choosing the playlist's destination.
                Button {
                    runPlaylistSavePanel(model: model,
                                         defaultName: "New Playlist") { stem, dir in
                        let id = model.mlSavePlaylistAs(name: stem,
                                                        trackPaths: [],
                                                        directory: dir)
                        if id >= 0 {
                            model.mlRefreshSavedPlaylists()
                            nav = .playlist(id: id)
                        }
                    }
                } label: {
                    Label("New Playlist", systemImage: "plus")
                        .font(theme.vars.bodyFont)
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .help("Create a new playlist file via Save panel")
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(theme.background)

            Divider().background(theme.windowBorder)

            if visiblePlaylists.isEmpty {
                Spacer()
                Text(model.mlSavedPlaylists.isEmpty
                     ? "No saved playlists yet.\nClick + to create one."
                     : "No playlists match your search.")
                    .multilineTextAlignment(.center)
                    .font(theme.vars.bodyFont)
                    .foregroundStyle(theme.playlistDurationText)
                Spacer()
            } else {
                List(visiblePlaylists) { pl in
                    HStack(spacing: 8) {
                        playlistIcon(pl)
                        Text(pl.name)
                            .font(theme.vars.bodyFont)
                            .foregroundStyle(theme.playlistText)
                        if let server = pl.serverName {
                            Text(server)
                                .font(.system(size: 10))
                                .foregroundStyle(theme.playlistDurationText)
                        }
                        Spacer()
                        if pl.isServer {
                            // Read-only until changes are sent to servers.
                            Image(systemName: "lock.fill")
                                .font(.system(size: 10))
                                .foregroundStyle(theme.playlistDurationText)
                                .help(pl.sourceNote)
                        } else {
                            Button {
                                renameTarget = pl.id
                                renameText   = pl.name
                                showingRename = true
                            } label: {
                                Image(systemName: "pencil").font(.system(size: 10))
                            }
                            .buttonStyle(.borderless)
                            .foregroundStyle(theme.playlistDurationText)
                            .help("Rename")

                            Button {
                                if nav == .playlist(id: pl.id) { nav = .playlists }
                                model.mlDeletePlaylist(id: pl.id)
                            } label: {
                                Image(systemName: "trash").font(.system(size: 10))
                            }
                            .buttonStyle(.borderless)
                            .foregroundStyle(.red)
                            .help("Delete")
                        }
                    }
                    .contentShape(Rectangle())
                    .listRowBackground(theme.playlistBg)
                    .onTapGesture { nav = .playlist(id: pl.id) }
                    // Drag source: the playlist id, exactly what the sidebar
                    // row publishes. A drop target reads it either as "sync
                    // this whole playlist" (a device) or expands it to its
                    // tracks (the active playlist) — the same `pl:<id>`
                    // contract GTK's `attach_pl_row_drag` uses, and GTK's
                    // `expand_playlist_drop` is where that expansion happens
                    // there.
                    //
                    // Deferred because expanding means a database read, and
                    // this runs the instant the gesture starts.
                    .onDrag {
                        SparkampDrag.begin(
                            // id 0 is a stub row — a path in the .m3u that
                            // the library has no record of. The editor's own
                            // Enqueue drops those too.
                            .deferred {
                                .libraryIds(model.mlGetPlaylistTracks(id: pl.id)
                                    .map(\.id).filter { $0 != 0 })
                            },
                            plainText: "sparkamp.playlist:\(pl.id)")
                    }
                }
                .listStyle(.plain)
                .background(theme.playlistBg)
                .scrollContentBackground(.hidden)
                .tint(theme.vars.highlight)
            }
        }
        .background(theme.playlistBg)
        .onAppear { model.mlRefreshSavedPlaylists() }
        .sheet(isPresented: $showingRename) {
            VStack(spacing: 16) {
                Text("Rename Playlist").font(.headline)
                TextField("Name", text: $renameText)
                    .textFieldStyle(.roundedBorder).frame(width: 260)
                HStack {
                    Button("Cancel") { showingRename = false }
                    Spacer()
                    Button("Rename") {
                        showingRename = false
                        if let id = renameTarget { model.mlRenamePlaylist(id: id, name: renameText) }
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(renameText.trimmingCharacters(in: .whitespaces).isEmpty)
                }
            }
            .padding(24).frame(width: 320)
        }
    }

    /// Where the playlist lives, once a server is set up; the plain playlist
    /// icon before, when every playlist is a file here.
    @ViewBuilder
    private func playlistIcon(_ pl: MLPlaylistItem) -> some View {
        if model.servers.isEmpty {
            Image(systemName: "play.rectangle")
                .font(.system(size: 10))
                .foregroundStyle(theme.playlistDurationText)
        } else {
            Image(pl.sourceIcon)
                .resizable()
                .frame(width: 16, height: 16)
                .help(pl.sourceNote)
        }
    }
}
