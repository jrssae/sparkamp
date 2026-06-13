import SwiftUI
import AppKit
import UniformTypeIdentifiers

// MARK: - Playlist management (nav = .playlists)

struct MLPlaylistManagement: View {
    @Binding var nav: MLNavigation
    let theme: SkinTheme

    @EnvironmentObject var model: SparkampModel

    @State private var showingRename = false
    @State private var renameText    = ""
    @State private var renameTarget: Int64? = nil

    var body: some View {
        VStack(spacing: 0) {
            // Header
            HStack {
                Text("Saved Playlists")
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

            if model.mlSavedPlaylists.isEmpty {
                Spacer()
                Text("No saved playlists yet.\nClick + to create one.")
                    .multilineTextAlignment(.center)
                    .font(theme.vars.bodyFont)
                    .foregroundStyle(theme.playlistDurationText)
                Spacer()
            } else {
                List(model.mlSavedPlaylists) { pl in
                    HStack(spacing: 8) {
                        Image(systemName: "play.rectangle")
                            .font(.system(size: 10))
                            .foregroundStyle(theme.playlistDurationText)
                        Text(pl.name)
                            .font(theme.vars.bodyFont)
                            .foregroundStyle(theme.playlistText)
                        Spacer()
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
                    .contentShape(Rectangle())
                    .listRowBackground(theme.playlistBg)
                    .onTapGesture { nav = .playlist(id: pl.id) }
                }
                .listStyle(.plain)
                .background(theme.playlistBg)
                .scrollContentBackground(.hidden)
                .tint(theme.vars.highlight)
            }
        }
        .background(theme.playlistBg)
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
}

