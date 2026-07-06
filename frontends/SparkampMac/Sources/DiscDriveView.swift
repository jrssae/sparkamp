import SwiftUI

/// Detail page for one optical drive: header (drive label + loaded-media
/// state + actions) and, for an audio CD, the track list with add-to-playlist
/// actions. Blank/data/no-disc states show an explanatory banner instead —
/// burning arrives in a later phase.
struct DiscDriveView: View {
    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager

    let drive: OpticalDrive
    let theme: SkinTheme

    @State private var selection: Set<Int> = []
    @State private var showTagEditor = false
    @State private var editTags = DiscTagSet()

    private var vars: SkinVars { themeManager.currentVars }
    private var isEjecting: Bool { model.ejectingDiscs.contains(drive.id) }
    /// The stored tag set for the loaded disc (nil until matched/edited).
    private var discTags: DiscTagSet? {
        model.discIdFor(drive).flatMap { model.discTagSets[$0] }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            if let s = model.discStatus {
                Text(s)
                    .font(.system(size: 11))
                    .foregroundStyle(theme.playlistDurationText)
                    .padding(.horizontal, 16)
                    .padding(.bottom, 8)
            }
            Divider().background(theme.windowBorder)
            if drive.media.isAudioCd {
                trackTable
                bottomBar
            } else {
                banner
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(theme.background)
        .onAppear { model.loadDiscTracks(drive) }
        .onChange(of: drive.id) { _, _ in
            selection.removeAll()
            model.loadDiscTracks(drive)
        }
        // Disc swapped/ejected under us — reload the entries.
        .onChange(of: drive.toc) { _, _ in
            selection.removeAll()
            model.loadDiscTracks(drive)
        }
        // gnudb offered several matches — let the user pick.
        .sheet(isPresented: Binding(
            get: { model.discMatches != nil },
            set: { if !$0 { model.discMatches = nil } }
        )) {
            matchSheet
        }
        .sheet(isPresented: $showTagEditor) {
            tagEditorSheet
        }
    }

    // MARK: gnudb match picker

    private var matchSheet: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("gnudb Matches")
                .font(.headline)
                .padding()
            Divider()
            List(model.discMatches ?? []) { m in
                Button {
                    model.chooseDiscMatch(drive, match: m)
                } label: {
                    HStack(spacing: 8) {
                        Text(m.exact ? "exact" : "close")
                            .font(.system(size: 10, weight: .medium))
                            .padding(.horizontal, 5)
                            .padding(.vertical, 2)
                            .background(
                                RoundedRectangle(cornerRadius: 3)
                                    .fill(m.exact ? Color.green.opacity(0.25)
                                                  : Color.yellow.opacity(0.25))
                            )
                        VStack(alignment: .leading, spacing: 1) {
                            Text(m.title).font(vars.bodyFont)
                            Text("\(m.category) · \(m.discid)")
                                .font(.system(size: 10))
                                .foregroundStyle(.secondary)
                        }
                        Spacer()
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
            Divider()
            HStack {
                Spacer()
                Button("Cancel") { model.discMatches = nil }
                    .keyboardShortcut(.cancelAction)
            }
            .padding()
        }
        .frame(width: 460, height: 360)
    }

    // MARK: Tag override editor

    private var tagEditorSheet: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("Disc Tags")
                .font(.headline)
                .padding()
            Divider()
            Form {
                TextField("Artist", text: $editTags.artist)
                TextField("Album", text: $editTags.album)
                TextField("Year", text: $editTags.year)
                TextField("Genre", text: $editTags.genre)
            }
            .padding(.horizontal)
            .padding(.top, 8)
            Divider().padding(.top, 8)
            ScrollView {
                VStack(spacing: 4) {
                    ForEach(editTags.titles.indices, id: \.self) { i in
                        HStack(spacing: 8) {
                            Text("\(i + 1)")
                                .font(vars.bodyFont.monospacedDigit())
                                .frame(width: 24, alignment: .trailing)
                                .foregroundStyle(.secondary)
                            TextField("Track \(i + 1)", text: $editTags.titles[i])
                                .textFieldStyle(.roundedBorder)
                        }
                    }
                }
                .padding(.horizontal)
                .padding(.vertical, 8)
            }
            Divider()
            HStack {
                Spacer()
                Button("Cancel") { showTagEditor = false }
                    .keyboardShortcut(.cancelAction)
                Button("Save") {
                    model.saveDiscTags(drive, tags: editTags)
                    showTagEditor = false
                }
                .keyboardShortcut(.defaultAction)
            }
            .padding()
        }
        .frame(width: 480, height: 520)
    }

    // MARK: Header

    @ViewBuilder
    private var header: some View {
        HStack(alignment: .center, spacing: 12) {
            Image(systemName: "opticaldiscdrive.fill")
                .font(.system(size: 30))
                .foregroundStyle(theme.vars.highlight)

            VStack(alignment: .leading, spacing: 2) {
                Text(drive.label)
                    .font(vars.bodyFont.weight(.semibold))
                    .foregroundStyle(theme.playlistText)
                    .lineLimit(1)
                Text(drive.mediaSummary)
                    .font(.system(size: 11))
                    .foregroundStyle(theme.playlistDurationText)
                    .lineLimit(1)
                // Identified disc: artist — album (year) under the media line.
                if let t = discTags, !t.artist.isEmpty || !t.album.isEmpty {
                    Text("\(t.artist)\(t.album.isEmpty ? "" : " — \(t.album)")\(t.year.isEmpty ? "" : " (\(t.year))")")
                        .font(.system(size: 11, weight: .medium))
                        .foregroundStyle(theme.playlistText)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
            }

            Spacer()

            HStack(spacing: 8) {
                if model.discBusy || model.discIdentifying {
                    ProgressView().controlSize(.small)
                }

                if drive.media.isAudioCd {
                    Button { model.identifyDisc(drive) } label: {
                        Label("Identify", systemImage: "magnifyingglass")
                    }
                    .disabled(model.discIdentifying)
                    .help("Look this disc up on gnudb.org")

                    Button {
                        editTags = model.discTagsForEditing(drive)
                        showTagEditor = true
                    } label: {
                        Label("Edit Tags", systemImage: "pencil")
                    }
                    .disabled(model.discTracks.isEmpty)
                    .help("Set artist/album and per-track titles (used for display and ripping)")
                }

                Button {
                    model.pollDiscDrives()
                    model.loadDiscTracks(drive)
                } label: {
                    Label("Scan", systemImage: "arrow.clockwise")
                }
                .disabled(model.discBusy)

                if isEjecting {
                    HStack(spacing: 6) {
                        ProgressView().controlSize(.small)
                        Text("Ejecting…").font(.system(size: 11))
                            .foregroundStyle(theme.playlistDurationText)
                    }
                } else if drive.media.present {
                    Button { model.ejectDisc(drive) } label: {
                        Label("Eject", systemImage: "eject")
                    }
                }
            }
            .buttonStyle(.bordered)
        }
        .padding(16)
    }

    // MARK: Track table

    private var trackTable: some View {
        Table(model.discTracks, selection: $selection) {
            TableColumn("#") { e in
                Text("\(e.number)")
                    .font(vars.bodyFont)
                    .foregroundStyle(theme.playlistText)
            }
            .width(min: 24, ideal: 30, max: 40)

            TableColumn("Title") { e in
                Text(e.title)
                    .font(vars.bodyFont)
                    .foregroundStyle(theme.playlistText)
            }

            TableColumn("Duration") { e in
                Text(e.durationText)
                    .font(vars.bodyFont.monospacedDigit())
                    .foregroundStyle(theme.playlistDurationText)
            }
            .width(min: 56, ideal: 70, max: 90)
        }
        .scrollContentBackground(.hidden)
        .background(theme.lcdBackground)
        .contextMenu(forSelectionType: Int.self) { ids in
            Button("Add to Playlist") { addSelected(ids) }
                .disabled(ids.isEmpty)
            Button("Add Whole Disc") { model.addDiscTracks(model.discTracks) }
        } primaryAction: { ids in
            // Double-click adds the clicked/selected tracks.
            addSelected(ids)
        }
    }

    private var bottomBar: some View {
        HStack(spacing: 10) {
            Text("\(model.discTracks.count) track\(model.discTracks.count == 1 ? "" : "s")")
                .font(.system(size: 11))
                .foregroundStyle(theme.playlistDurationText)
            Spacer()
            Button("Add Selected") { addSelected(selection) }
                .disabled(selection.isEmpty)
            Button("Add All") { model.addDiscTracks(model.discTracks) }
                .disabled(model.discTracks.isEmpty)
        }
        .buttonStyle(.bordered)
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
        .background(theme.background)
    }

    private func addSelected(_ ids: Set<Int>) {
        let entries = model.discTracks.filter { ids.contains($0.number) }
        model.addDiscTracks(entries)
    }

    // MARK: Non-audio banner

    private var banner: some View {
        let (icon, title, detail): (String, String, String) = {
            if !drive.media.present {
                return ("opticaldisc", "No disc",
                        "Insert an audio CD to play or rip its tracks.")
            }
            if drive.media.isBlank {
                return ("opticaldisc", "Blank \(drive.media.kind.displayName)",
                        "Burning arrives in a later phase.")
            }
            return ("opticaldisc", "Data disc",
                    "This disc holds data, not CD audio. Audio files on it appear under Devices when the volume mounts.")
        }()
        return VStack(spacing: 8) {
            Image(systemName: icon)
                .font(.system(size: 32))
                .foregroundStyle(theme.playlistDurationText)
            Text(title)
                .font(vars.bodyFont.weight(.semibold))
                .foregroundStyle(theme.playlistText)
            Text(detail)
                .font(vars.bodyFont)
                .foregroundStyle(theme.playlistDurationText)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 380)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(40)
    }
}
