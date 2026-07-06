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

    private var vars: SkinVars { themeManager.currentVars }
    private var isEjecting: Bool { model.ejectingDiscs.contains(drive.id) }

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
            }

            Spacer()

            HStack(spacing: 8) {
                if model.discBusy { ProgressView().controlSize(.small) }

                if drive.media.isAudioCd {
                    Button {
                        model.addDiscTracks(model.discTracks)
                    } label: {
                        Label("Add Disc", systemImage: "plus")
                    }
                    .disabled(model.discTracks.isEmpty || model.discBusy)
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
