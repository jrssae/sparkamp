import AppKit
import SwiftUI

// MARK: - Disc media icon

/// A disc glyph with the loaded media's format badged on it ("CD", "CD-R",
/// "DVD-RW"…); the bare drive glyph when the tray is empty. Shared by the
/// drive detail header and the Disc Drives overview cards — visually
/// distinct from the removable-device (externaldrive) icon.
struct DiscMediaIcon: View {
    let drive: OpticalDrive
    let size: CGFloat
    let theme: SkinTheme

    /// Short format label; nil with an empty tray. Pressed discs don't
    /// report a writable kind, so CD vs DVD falls back to a capacity
    /// heuristic (>1 GB = DVD).
    private var badge: String? {
        guard drive.media.present else { return nil }
        if drive.media.isAudioCd { return "CD" }
        switch drive.media.kind {
        case .unknown:
            return drive.media.capacityBytes > 1_000_000_000 ? "DVD" : "CD"
        default:
            return drive.media.kind.displayName
        }
    }

    var body: some View {
        ZStack(alignment: .bottomTrailing) {
            Image(systemName: drive.media.present ? "opticaldisc.fill" : "opticaldiscdrive")
                .font(.system(size: size))
                .foregroundStyle(theme.vars.highlight)
            if let badge = badge {
                Text(badge)
                    .font(.system(size: max(6, size * 0.23), weight: .bold))
                    .padding(.horizontal, 3)
                    .padding(.vertical, 1)
                    .background(RoundedRectangle(cornerRadius: 3).fill(theme.background))
                    .overlay(
                        RoundedRectangle(cornerRadius: 3)
                            .stroke(theme.vars.highlight, lineWidth: 0.5)
                    )
                    .foregroundStyle(theme.vars.highlight)
                    .offset(x: 5, y: 4)
            }
        }
    }
}

// MARK: - Disc Drives overview (grid of drive cards)

/// Overview page for the "Disc Drives" sidebar group, in the style of the
/// Devices overview: one card per physical drive; tapping a card opens that
/// drive's detail view.
struct DiscOverview: View {
    let drives: [OpticalDrive]
    let theme: SkinTheme
    let vars: SkinVars
    let onSelect: (OpticalDrive) -> Void
    /// Non-nil after a viewed drive disconnected: shown as a dismissible banner.
    var disconnectNotice: String? = nil
    var onDismissNotice: (() -> Void)? = nil

    private let columns = [GridItem(.adaptive(minimum: 240), spacing: 16)]

    var body: some View {
        ScrollView {
            if let notice = disconnectNotice {
                HStack(spacing: 8) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .foregroundStyle(.orange)
                    Text(notice)
                        .font(vars.bodyFont)
                        .foregroundStyle(theme.playlistText)
                    Spacer()
                    Button {
                        onDismissNotice?()
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                            .foregroundStyle(theme.playlistDurationText)
                    }
                    .buttonStyle(.plain)
                }
                .padding(10)
                .background(
                    RoundedRectangle(cornerRadius: 8).fill(Color.orange.opacity(0.15))
                )
                .padding([.horizontal, .top], 12)
            }
            if drives.isEmpty {
                VStack(spacing: 8) {
                    Image(systemName: "opticaldiscdrive")
                        .font(.system(size: 36))
                        .foregroundStyle(theme.playlistDurationText)
                    Text("No disc drives connected")
                        .font(vars.bodyFont.weight(.semibold))
                        .foregroundStyle(theme.playlistText)
                    Text("Connect an optical drive to play, rip, or burn discs.")
                        .font(vars.bodyFont)
                        .foregroundStyle(theme.playlistDurationText)
                }
                .frame(maxWidth: .infinity, minHeight: 240)
                .padding(40)
            } else {
                LazyVGrid(columns: columns, spacing: 16) {
                    ForEach(drives) { drive in
                        card(drive)
                    }
                }
                .padding(16)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(theme.background)
    }

    /// "45:12 of audio" for an audio CD; free/total for other media; a hint
    /// when the tray is empty.
    private func detailLine(_ drive: OpticalDrive) -> String {
        if drive.media.isAudioCd, let toc = drive.toc {
            let first = toc.tracks.first?.startFrame ?? 0
            let secs = Int(toc.leadoutFrame > first ? (toc.leadoutFrame - first) / 75 : 0)
            return String(format: "%d:%02d of audio", secs / 60, secs % 60)
        }
        if drive.media.present, drive.media.capacityBytes > 0 {
            let f = ByteCountFormatter()
            f.countStyle = .file
            let free = f.string(fromByteCount: Int64(drive.media.freeBytes))
            let total = f.string(fromByteCount: Int64(drive.media.capacityBytes))
            return drive.media.isBlank ? "\(total) writable" : "\(free) free of \(total)"
        }
        return drive.media.present ? "—" : "Insert a disc to play, rip, or burn"
    }

    @ViewBuilder
    private func card(_ drive: OpticalDrive) -> some View {
        Button { onSelect(drive) } label: {
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 8) {
                    DiscMediaIcon(drive: drive, size: 22, theme: theme)
                    Text(drive.label)
                        .font(vars.bodyFont.weight(.semibold))
                        .foregroundStyle(theme.playlistText)
                        .lineLimit(1)
                    Spacer()
                }

                Text(drive.mediaSummary)
                    .font(.system(size: 11))
                    .foregroundStyle(theme.playlistDurationText)

                Text(detailLine(drive))
                    .font(.system(size: 11))
                    .foregroundStyle(theme.playlistDurationText)
            }
            .padding(12)
            .background(
                RoundedRectangle(cornerRadius: 8).fill(theme.playlistCurrentBg.opacity(0.5))
            )
            .overlay(
                RoundedRectangle(cornerRadius: 8).stroke(theme.windowBorder, lineWidth: 1)
            )
        }
        .buttonStyle(.plain)
    }
}

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
    @State private var searchText = ""
    @State private var showTagEditor = false
    @State private var editTags = DiscTagSet()
    @State private var showSubmit = false
    @State private var submitCategory = "misc"
    // Rip sheet state: which tracks, where to, what quality.
    @State private var showRip = false
    @State private var ripSelection: Set<Int> = []
    @State private var ripDest = ""
    @State private var ripQuality = 1
    // First-submission email capture (gnudb requires a personal address; the
    // config ships blank on purpose).
    @State private var showEmailPrompt = false
    @State private var emailInput = ""

    private var vars: SkinVars { themeManager.currentVars }
    private var isEjecting: Bool { model.ejectingDiscs.contains(drive.id) }
    /// The stored tag set for the loaded disc (nil until matched/edited).
    private var discTags: DiscTagSet? {
        model.discIdFor(drive).flatMap { model.discTagSets[$0] }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            if let rp = model.ripProgress {
                // Rip in flight: per-track progress + stop-after-this-track.
                VStack(alignment: .leading, spacing: 3) {
                    // Finished tracks + progress within the current one, so
                    // the bar moves during a single track too.
                    ProgressView(
                        value: min(Double(rp.done) + model.ripTrackFrac, Double(rp.total)),
                        total: Double(max(rp.total, 1)))
                    HStack {
                        Text("Ripping \(min(rp.done + 1, rp.total))/\(rp.total) · \(rp.name) (\(Int(model.ripTrackFrac * 100))%)")
                            .font(.system(size: 11))
                            .foregroundStyle(theme.playlistDurationText)
                            .lineLimit(1)
                            .truncationMode(.middle)
                        Spacer()
                        Button(model.ripCancelRequested ? "Stopping…" : "Cancel") {
                            model.ripCancelRequested = true
                        }
                        .buttonStyle(.borderless)
                        .font(.system(size: 11))
                        .disabled(model.ripCancelRequested)
                    }
                }
                .padding(.horizontal, 16)
                .padding(.bottom, 8)
            } else if let s = model.discStatus {
                Text(s)
                    .font(.system(size: 11))
                    .foregroundStyle(theme.playlistDurationText)
                    .padding(.horizontal, 16)
                    .padding(.bottom, 8)
            }
            if let phase = model.burnPhase {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text(phase)
                        .font(.system(size: 11))
                        .foregroundStyle(theme.playlistDurationText)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Spacer()
                    Button("Cancel") { model.cancelBurn() }
                        .buttonStyle(.borderless)
                        .font(.system(size: 11))
                }
                .padding(.horizontal, 16)
                .padding(.bottom, 8)
            }
            Divider().background(theme.windowBorder)
            if drive.media.isAudioCd {
                searchBar
                trackTable
                bottomBar
            } else if drive.media.present {
                burnPanel
            } else {
                banner
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(theme.background)
        .onAppear { model.loadDiscTracks(drive) }
        .onChange(of: drive.id) { _, _ in
            selection.removeAll()
            searchText = ""
            model.loadDiscTracks(drive)
        }
        // Disc swapped/ejected under us — reload the entries.
        .onChange(of: drive.toc) { _, _ in
            selection.removeAll()
            model.loadDiscTracks(drive)
        }
        // gnudb offered several matches — let the user pick. Presented only
        // on the drive the lookup was for (results survive window close /
        // navigation and re-present here, never on an unrelated drive).
        .sheet(isPresented: Binding(
            get: { model.discMatches != nil && model.discMatchesDriveId == drive.id },
            set: { presented in
                if !presented {
                    model.discMatches = nil
                    model.discMatchesDriveId = nil
                }
            }
        )) {
            matchSheet
        }
        .sheet(isPresented: $showTagEditor) {
            tagEditorSheet
        }
        .sheet(isPresented: $showSubmit) {
            submitSheet
        }
        .sheet(isPresented: $showEmailPrompt) {
            emailPromptSheet
        }
        .sheet(isPresented: $showRip) {
            ripSheet
        }
    }

    // MARK: Rip

    /// Whether the chosen destination sits under a watched folder — outside
    /// one, the import step skips the files (library policy: importing never
    /// creates new watch folders), so the sheet warns.
    private var ripDestWatched: Bool {
        model.mlFolders.contains { ripDest.hasPrefix($0) }
    }

    /// Prefill the rip sheet: tracks from the table selection (or all),
    /// destination from config → first watched folder → ~/Music, quality
    /// from config.
    private func openRipSheet() {
        if model.mlIsOpen { model.mlRefreshFolders() }
        // Every track starts selected — ripping the whole disc is the common
        // case; Select All / Deselect All in the sheet handle the rest.
        ripSelection = Set(model.discTracks.map(\.number))
        if let ctx = model.ctx {
            let p = sparkamp_get_rip_dest(ctx)
            ripDest = p.map { String(cString: $0) } ?? ""
            sparkamp_free_string(p)
            ripQuality = Int(sparkamp_get_rip_quality(ctx))
        }
        if ripDest.isEmpty {
            ripDest = model.mlFolders.first
                ?? (NSHomeDirectory() + "/Music")
        }
        showRip = true
    }

    private var ripSheet: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Rip to MP3")
                .font(.headline)
            Text("Encodes the chosen tracks as \(["V0 (~245 kbps)", "V2 (~190 kbps)", "320 kbps CBR"][min(ripQuality, 2)]) MP3s under Artist/Album, tagged from the disc's tags, then adds them to the Media Library.")
                .font(vars.bodyFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            // Track picker.
            ScrollView {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(model.discTracks) { e in
                        Toggle(isOn: Binding(
                            get: { ripSelection.contains(e.number) },
                            set: { on in
                                if on { ripSelection.insert(e.number) }
                                else { ripSelection.remove(e.number) }
                            }
                        )) {
                            Text("\(e.number). \(e.title)")
                                .font(vars.bodyFont)
                                .lineLimit(1)
                        }
                        .toggleStyle(.checkbox)
                    }
                }
            }
            .frame(maxHeight: 180)

            HStack(spacing: 8) {
                Text("Into:")
                Text(ripDest)
                    .font(vars.bodyFont)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .foregroundStyle(.secondary)
                Button("Choose…") { chooseRipDest() }
            }
            if !ripDestWatched {
                Label("Not a watched folder — the files will rip here but won't appear in the Media Library.",
                      systemImage: "exclamationmark.triangle")
                    .font(.system(size: 11))
                    .foregroundStyle(.yellow)
            }

            Picker("Quality", selection: $ripQuality) {
                Text("VBR V0 (~245 kbps)").tag(0)
                Text("VBR V2 (~190 kbps)").tag(1)
                Text("320 kbps CBR").tag(2)
            }

            HStack {
                Text("\(ripSelection.count) of \(model.discTracks.count) tracks")
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
                Button("Select All") {
                    ripSelection = Set(model.discTracks.map(\.number))
                }
                .buttonStyle(.borderless)
                .font(.system(size: 11))
                Button("Deselect All") { ripSelection.removeAll() }
                    .buttonStyle(.borderless)
                    .font(.system(size: 11))
                Spacer()
                Button("Cancel") { showRip = false }
                    .keyboardShortcut(.cancelAction)
                Button("Rip") {
                    showRip = false
                    let entries = model.discTracks.filter { ripSelection.contains($0.number) }
                    model.ripDiscTracks(
                        drive, entries: entries, destRoot: ripDest, quality: ripQuality)
                }
                .keyboardShortcut(.defaultAction)
                .disabled(ripSelection.isEmpty || ripDest.isEmpty)
            }
        }
        .padding(20)
        .frame(width: 460)
    }

    private func chooseRipDest() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.canCreateDirectories = true
        panel.allowsMultipleSelection = false
        panel.directoryURL = URL(fileURLWithPath: ripDest, isDirectory: true)
        if panel.runModal() == .OK, let url = panel.url {
            ripDest = url.path
        }
    }

    // MARK: gnudb submission

    private func currentGnudbEmail() -> String {
        guard let ctx = model.ctx else { return "" }
        let p = sparkamp_get_gnudb_email(ctx)
        defer { sparkamp_free_string(p) }
        return p.map { String(cString: $0) } ?? ""
    }

    /// Deliverable-shape check — the core's shared rule (x@y.z), so every
    /// frontend enforces the same thing.
    private var emailLooksValid: Bool {
        emailInput.withCString { sparkamp_gnudb_email_valid(nil, $0) }
    }

    private var emailPromptSheet: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Your email for gnudb")
                .font(.headline)
            Text("gnudb requires each submission to carry the submitter's own email address (never an app-wide default). It's sent only with submissions and can be changed later in Settings → Media Library.")
                .font(vars.bodyFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            TextField("you@example.com", text: $emailInput)
                .textFieldStyle(.roundedBorder)
                .autocorrectionDisabled()
                .onSubmit { saveEmailAndContinue() }
            HStack {
                Spacer()
                Button("Cancel") { showEmailPrompt = false }
                    .keyboardShortcut(.cancelAction)
                Button("Save & Continue") { saveEmailAndContinue() }
                    .keyboardShortcut(.defaultAction)
                    .disabled(!emailLooksValid)
            }
        }
        .padding(20)
        .frame(width: 420)
    }

    private func saveEmailAndContinue() {
        guard emailLooksValid, let ctx = model.ctx else { return }
        emailInput.trimmingCharacters(in: .whitespaces)
            .withCString { sparkamp_set_gnudb_email(ctx, $0) }
        showEmailPrompt = false
        showSubmit = true
    }

    private var submitSheet: some View {
        let testMode = model.ctx.map { sparkamp_get_gnudb_submit_test($0) } ?? true
        return VStack(alignment: .leading, spacing: 12) {
            Text("Submit to gnudb")
                .font(.headline)
            Text("Sends this disc's TOC and tags to gnudb.org so other players can identify it. gnudb requires one of its fixed categories.")
                .font(vars.bodyFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            // Typeahead over the fixed category set; Submit stays disabled
            // until the text is one of them.
            HStack(alignment: .top, spacing: 8) {
                Text("Category")
                    .font(vars.bodyFont)
                TypeaheadTextField(
                    placeholder: "misc",
                    text: $submitCategory,
                    items: gnudbCategories,
                    font: vars.bodyFont)
            }
            if !gnudbCategories.contains(submitCategory) {
                Label("Pick one of gnudb's categories: \(gnudbCategories.joined(separator: ", "))",
                      systemImage: "exclamationmark.triangle")
                    .font(.system(size: 11))
                    .foregroundStyle(.yellow)
            }
            if testMode {
                Label("Test mode: gnudb validates the entry but doesn't publish it. Turn this off in Settings → Media Library once a submission is confirmed.",
                      systemImage: "info.circle")
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
            }
            HStack {
                Spacer()
                Button("Cancel") { showSubmit = false }
                    .keyboardShortcut(.cancelAction)
                Button(testMode ? "Submit (test)" : "Submit") {
                    showSubmit = false
                    model.submitDisc(drive, category: submitCategory)
                }
                .keyboardShortcut(.defaultAction)
                .disabled(!gnudbCategories.contains(submitCategory))
            }
        }
        .padding(20)
        .frame(width: 420)
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
            DiscMediaIcon(drive: drive, size: 30, theme: theme)

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

                    Button { openRipSheet() } label: {
                        Label("Rip…", systemImage: "square.and.arrow.down")
                    }
                    .disabled(model.discTracks.isEmpty || model.ripProgress != nil)
                    .help("Encode tracks to tagged MP3s and add them to the library")

                    Button {
                        editTags = model.discTagsForEditing(drive)
                        showTagEditor = true
                    } label: {
                        Label("Edit Tags", systemImage: "pencil")
                    }
                    .disabled(model.discTracks.isEmpty)
                    .help("Set artist/album and per-track titles (used for display and ripping)")

                    // Shown for a disc gnudb doesn't know, or once the tags
                    // differ from the official match — the path for feeding
                    // corrections back upstream.
                    if model.discSubmittable(drive) {
                        Button {
                            let tags = model.discTagsForEditing(drive)
                            submitCategory = suggestGnudbCategory(for: tags.genre)
                            // gnudb requires a personal address — capture it
                            // once before the first submission.
                            if !currentGnudbEmail().withCString({
                                sparkamp_gnudb_email_valid(nil, $0)
                            }) {
                                emailInput = ""
                                showEmailPrompt = true
                            } else {
                                showSubmit = true
                            }
                        } label: {
                            Label("Submit to gnudb", systemImage: "square.and.arrow.up")
                        }
                        .disabled(model.discSubmitting)
                        .help("Send this disc's tags to gnudb.org so other players can identify it")
                    }
                }

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

    /// The disc's tracks matching the per-view search (all of them while the
    /// query is empty). Add All / Add Whole Disc stay unfiltered on purpose.
    private var filteredTracks: [DiscTrackEntry] {
        searchText.isEmpty
            ? model.discTracks
            : model.discTracks.filter { $0.title.localizedCaseInsensitiveContains(searchText) }
    }

    /// Per-view search over just this disc's track list — same styling as the
    /// Files view search field.
    private var searchBar: some View {
        HStack(spacing: 4) {
            Image(systemName: "magnifyingglass")
                .foregroundStyle(theme.playlistDurationText)
                .font(.system(size: 11))
            TextField("Search this disc…", text: $searchText)
                .textFieldStyle(.plain)
                .font(vars.bodyFont)
                .foregroundStyle(theme.playlistText)
            if !searchText.isEmpty {
                Button { searchText = "" } label: {
                    Image(systemName: "xmark.circle.fill")
                        .foregroundStyle(theme.playlistDurationText)
                        .font(.system(size: 11))
                }
                .buttonStyle(.plain)
            }
        }
        .padding(4)
        .background(theme.lcdBackground.opacity(0.8))
        .cornerRadius(6)
        .overlay(RoundedRectangle(cornerRadius: 6).stroke(theme.windowBorder, lineWidth: 1))
        .padding(.horizontal, 16)
        .padding(.vertical, 6)
    }

    private var trackTable: some View {
        Table(filteredTracks, selection: $selection) {
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
            Button("Add Whole Disc") { model.addDiscTracks(drive, entries: model.discTracks) }
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
            Button("Add All") { model.addDiscTracks(drive, entries: model.discTracks) }
                .disabled(model.discTracks.isEmpty)
        }
        .buttonStyle(.bordered)
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
        .background(theme.background)
    }

    private func addSelected(_ ids: Set<Int>) {
        let entries = model.discTracks.filter { ids.contains($0.number) }
        model.addDiscTracks(drive, entries: entries)
    }

    // MARK: Burn panel (blank / rewritable / data media)

    @State private var showEraseConfirm = false
    /// Which burn runs after the erase confirmation: true = audio.
    @State private var pendingBurnAudio = true

    private var burnPanel: some View {
        let decision = DiscService.eraseDecision(drive: drive)
        let capacitySecs = DiscService.audioCapacitySecs(drive: drive)
        let totalSecs = model.burnListTotalSecs
        let totalBytes = model.burnListTotalBytes
        let freeBytes = drive.media.freeBytes
        let overAudio = totalSecs > capacitySecs
        let overData = freeBytes > 0 && totalBytes > freeBytes
        let fmt = ByteCountFormatter()

        return VStack(alignment: .leading, spacing: 10) {
            Text("Burn List")
                .font(vars.bodyFont.weight(.semibold))
                .foregroundStyle(theme.playlistText)

            if model.burnList.isEmpty {
                Text("Queue tracks from the Media Library Files view: right-click → Add to Burn List.")
                    .font(vars.bodyFont)
                    .foregroundStyle(theme.playlistDurationText)
            } else {
                List {
                    ForEach(model.burnList) { e in
                        HStack {
                            Text(e.display)
                                .font(vars.bodyFont)
                                .lineLimit(1)
                                .truncationMode(.middle)
                            Spacer()
                            if let d = e.durationSecs {
                                Text(String(format: "%d:%02d", d / 60, d % 60))
                                    .font(vars.bodyFont.monospacedDigit())
                                    .foregroundStyle(theme.playlistDurationText)
                            }
                        }
                    }
                    .onDelete { model.removeFromBurnList(at: $0) }
                }
                .frame(minHeight: 120, maxHeight: 220)
                .scrollContentBackground(.hidden)
                .background(theme.lcdBackground)

                HStack(spacing: 16) {
                    Text(String(format: "Audio: %d:%02d of %d:%02d",
                                totalSecs / 60, totalSecs % 60,
                                capacitySecs / 60, capacitySecs % 60))
                        .foregroundStyle(overAudio ? Color.red : theme.playlistDurationText)
                    Text("Data: \(fmt.string(fromByteCount: Int64(totalBytes)))\(freeBytes > 0 ? " of \(fmt.string(fromByteCount: Int64(freeBytes)))" : "")")
                        .foregroundStyle(overData ? Color.red : theme.playlistDurationText)
                }
                .font(.system(size: 11))
            }

            switch decision {
            case 2:
                Label("This disc already has content and can't be rewritten — insert a blank or rewritable disc.",
                      systemImage: "exclamationmark.triangle")
                    .font(.system(size: 11))
                    .foregroundStyle(.yellow)
            case 1:
                Label("The disc has content; burning will erase it first (you'll be asked to confirm).",
                      systemImage: "info.circle")
                    .font(.system(size: 11))
                    .foregroundStyle(theme.playlistDurationText)
            default:
                EmptyView()
            }

            HStack(spacing: 8) {
                Button {
                    if decision == 1 {
                        pendingBurnAudio = true
                        showEraseConfirm = true
                    } else {
                        model.burnAudio(drive, eraseFirst: false)
                    }
                } label: {
                    Label("Burn Audio CD", systemImage: "opticaldisc")
                }
                .disabled(model.burnList.isEmpty || decision == 2 || overAudio
                          || model.burnPhase != nil)
                .help(overAudio ? "Over the disc's audio capacity — remove tracks first" : "")

                Button {
                    if decision == 1 {
                        pendingBurnAudio = false
                        showEraseConfirm = true
                    } else {
                        model.burnData(drive, eraseFirst: false)
                    }
                } label: {
                    Label("Burn Data Disc", systemImage: "doc.on.doc")
                }
                .disabled(model.burnList.isEmpty || decision == 2 || overData
                          || model.burnPhase != nil)
                .help(overData ? "Over the disc's free space — remove files first" : "")

                Spacer()

                Button("Clear List") { model.burnList.removeAll() }
                    .disabled(model.burnList.isEmpty || model.burnPhase != nil)
            }
            .buttonStyle(.bordered)

            Spacer()
        }
        .padding(16)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .confirmationDialog(
            "Erase this disc and burn?",
            isPresented: $showEraseConfirm, titleVisibility: .visible
        ) {
            Button("Erase & Burn", role: .destructive) {
                if pendingBurnAudio {
                    model.burnAudio(drive, eraseFirst: true)
                } else {
                    model.burnData(drive, eraseFirst: true)
                }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Everything currently on the disc is destroyed first. This can't be undone.")
        }
    }

    // MARK: Non-audio banner

    private var banner: some View {
        let (icon, title, detail) = (
            "opticaldisc", "No disc",
            "Insert an audio CD to play or rip, or a blank disc to burn."
        )
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
