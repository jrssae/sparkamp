import Foundation

// MARK: - Optical-disc operations
//
// State lives in SparkampModel (extensions can't hold stored properties).
// Detection and track listing shell out to drutil/plutil inside the core, so
// both always run on a background queue and hop back to the main actor —
// mirroring the device-sync threading model.

extension SparkampModel {

    /// Re-enumerate optical drives (background) and publish changes. Also
    /// clears a stale drive selection the same way pollDevices does.
    func pollDiscDrives() {
        DispatchQueue.global(qos: .utility).async {
            let drives = DiscService.listDrives()
            DispatchQueue.main.async {
                if drives != self.discDrives {
                    self.discDrives = drives
                }
            }
        }
    }

    /// Load the playlist-ready track entries for one drive's disc into
    /// `discTracks` (background; empty when no audio disc), then overlay any
    /// stored tag-set titles for this disc.
    func loadDiscTracks(_ drive: OpticalDrive) {
        discBusy = true
        DispatchQueue.global(qos: .userInitiated).async {
            let entries = DiscService.trackEntries(drive: drive)
            DispatchQueue.main.async {
                self.discTracks = entries
                self.discBusy = false
                self.applyDiscTagTitles(drive)
            }
        }
    }

    /// The freedb id of the drive's loaded disc, or nil without an audio disc.
    func discIdFor(_ drive: OpticalDrive) -> String? {
        drive.toc.flatMap { DiscService.discId(toc: $0) }
    }

    /// Overlay the stored tag set's titles onto `discTracks` ("Track N" stays
    /// wherever a title is missing/empty).
    func applyDiscTagTitles(_ drive: OpticalDrive) {
        guard let id = discIdFor(drive), let tags = discTagSets[id] else { return }
        discTracks = discTracks.map { entry in
            var e = entry
            let i = entry.number - 1
            if i >= 0 && i < tags.titles.count && !tags.titles[i].isEmpty {
                e.title = tags.titles[i]
            }
            return e
        }
    }

    /// Store an edited tag set for the drive's disc and refresh the titles.
    func saveDiscTags(_ drive: OpticalDrive, tags: DiscTagSet) {
        guard let id = discIdFor(drive) else { return }
        discTagSets[id] = tags
        applyDiscTagTitles(drive)
        discStatus = "Tags saved for this disc"
    }

    /// The current (or blank) tag set for the drive's disc, sized to its
    /// track count — what the editor sheet starts from.
    func discTagsForEditing(_ drive: OpticalDrive) -> DiscTagSet {
        let count = drive.toc?.tracks.count ?? discTracks.count
        var tags = discIdFor(drive).flatMap { discTagSets[$0] } ?? DiscTagSet()
        // Prefill from the visible entries so an editor without a match still
        // shows "Track N" placeholders sized correctly.
        if tags.titles.count < count {
            let existing = tags.titles
            tags.titles = (0..<count).map { i in
                i < existing.count && !existing[i].isEmpty
                    ? existing[i]
                    : (discTracks.first(where: { $0.number == i + 1 })?.title ?? "")
            }
        }
        return tags
    }

    /// Ask gnudb to identify the drive's disc. No match → status line; one
    /// exact match → applied immediately; several → `discMatches` sheet.
    func identifyDisc(_ drive: OpticalDrive) {
        guard let toc = drive.toc, !discIdentifying, let ctx = ctx else { return }
        let emailPtr = sparkamp_get_gnudb_email(ctx)
        let email = emailPtr.map { String(cString: $0) } ?? ""
        sparkamp_free_string(emailPtr)
        discIdentifying = true
        discStatus = nil
        // .utility, not .userInitiated: minreq blocks the calling thread on a
        // Default-QoS Rust worker, so any higher QoS here is a priority
        // inversion (Thread Performance Checker flags it). Utility is also
        // the intended class for network fetches with visible progress.
        DispatchQueue.global(qos: .utility).async {
            let result = DiscService.gnudbQuery(toc: toc, email: email)
            DispatchQueue.main.async {
                switch result {
                case .failure(let err):
                    self.discIdentifying = false
                    self.discStatus = err.message
                case .success(let matches) where matches.isEmpty:
                    self.discIdentifying = false
                    self.discStatus = "No gnudb match — use Edit Tags to fill titles in"
                case .success(let matches):
                    let exact = matches.filter { $0.exact }
                    if exact.count == 1, matches.count == 1 {
                        self.fetchDiscEntry(drive, match: exact[0], email: email)
                    } else {
                        self.discIdentifying = false
                        self.discMatches = matches
                    }
                }
            }
        }
    }

    /// User picked a match from the sheet (or the single exact match).
    func chooseDiscMatch(_ drive: OpticalDrive, match: DiscMatch) {
        guard let ctx = ctx else { return }
        let emailPtr = sparkamp_get_gnudb_email(ctx)
        let email = emailPtr.map { String(cString: $0) } ?? ""
        sparkamp_free_string(emailPtr)
        discMatches = nil
        fetchDiscEntry(drive, match: match, email: email)
    }

    private func fetchDiscEntry(_ drive: OpticalDrive, match: DiscMatch, email: String) {
        discIdentifying = true
        // .utility — same inversion note as identifyDisc.
        DispatchQueue.global(qos: .utility).async {
            let result = DiscService.gnudbRead(
                category: match.category, discid: match.discid, email: email)
            DispatchQueue.main.async {
                self.discIdentifying = false
                switch result {
                case .failure(let err):
                    self.discStatus = err.message
                case .success(let entry):
                    // Keep the untouched match as the submission baseline.
                    if let id = self.discIdFor(drive) {
                        self.discOfficial[id] = entry
                    }
                    let tags = DiscTagSet(
                        artist: entry.artist,
                        album: entry.album,
                        year: entry.year,
                        genre: entry.genre,
                        titles: entry.trackTitles)
                    self.saveDiscTags(drive, tags: tags)
                    self.discStatus = "\(entry.artist) — \(entry.album)"
                }
            }
        }
    }

    /// Whether the drive's disc has anything worth submitting to gnudb:
    /// always true for a disc gnudb doesn't know; for a matched disc, true
    /// only once the user's tags differ from the official entry.
    func discSubmittable(_ drive: OpticalDrive) -> Bool {
        guard drive.media.isAudioCd, let id = discIdFor(drive) else { return false }
        guard let official = discOfficial[id] else { return true }
        guard let tags = discTagSets[id] else { return false }
        let officialTags = DiscTagSet(
            artist: official.artist,
            album: official.album,
            year: official.year,
            genre: official.genre,
            titles: official.trackTitles)
        return tags != officialTags
    }

    /// Validate + POST the disc's tags to gnudb with the chosen category.
    /// Revision: official match + 1, or 0 for a new disc. Honors the
    /// test-mode setting (validated but not published) until it's turned off.
    func submitDisc(_ drive: OpticalDrive, category: String) {
        guard let toc = drive.toc, let id = discIdFor(drive),
              let tags = discTagSets[id], let ctx = ctx, !discSubmitting else { return }
        let emailPtr = sparkamp_get_gnudb_email(ctx)
        let email = emailPtr.map { String(cString: $0) } ?? ""
        sparkamp_free_string(emailPtr)
        let testMode = sparkamp_get_gnudb_submit_test(ctx)
        let entry = XmcdEntry(
            discid: id,
            artist: tags.artist,
            album: tags.album,
            year: tags.year,
            genre: tags.genre,
            trackTitles: tags.titles,
            extd: "",
            extt: [],
            revision: discOfficial[id].map { $0.revision + 1 } ?? 0)
        discSubmitting = true
        discStatus = testMode ? "Submitting to gnudb (test mode)…" : "Submitting to gnudb…"
        // .utility — same inversion note as identifyDisc.
        DispatchQueue.global(qos: .utility).async {
            let result = DiscService.gnudbSubmit(
                toc: toc, entry: entry, category: category, email: email, testMode: testMode)
            DispatchQueue.main.async {
                self.discSubmitting = false
                switch result {
                case .failure(let err):
                    self.discStatus = err.message
                case .success(let msg):
                    self.discStatus = testMode
                        ? "gnudb: \(msg) (test mode — not published)"
                        : "gnudb: \(msg)"
                }
            }
        }
    }

    /// Add disc tracks to the active playlist with their TOC titles and
    /// durations ("Track N" until gnudb supplies real names — Phase 2). No
    /// metadata scan or duration probe: the AIFFs carry no tags and the
    /// durations are already exact.
    ///
    /// Mirrors `mlDoubleClickTracks` semantics: honors the replace/append
    /// add-behavior setting, and autoplay-on-add starts the first new track
    /// when the playlist was replaced or was empty (never interrupts a track
    /// already playing).
    func addDiscTracks(_ entries: [DiscTrackEntry]) {
        guard let ctx = ctx, !entries.isEmpty else { return }
        let shouldReplace = Int(sparkamp_get_playlist_add_behavior(ctx)) == 1
        let autoplay = sparkamp_get_autoplay_on_add(ctx)
        if shouldReplace { clearPlaylist() }
        let indexBefore = Int(sparkamp_playlist_len(ctx))
        let wasEmpty = indexBefore == 0
        for e in entries {
            e.path.withCString { p in
                e.title.withCString { t in
                    _ = sparkamp_playlist_add_entry(ctx, p, t, Int32(e.durationSecs))
                }
            }
        }
        if autoplay && wasEmpty {
            sparkamp_playlist_jump(ctx, Int32(indexBefore))
            sparkamp_play(ctx)
        }
        refreshPlaylist()
        refreshCurrentTrackInfo()
        discStatus = "Added \(entries.count) disc track\(entries.count == 1 ? "" : "s")"
    }

    /// Eject the disc in a drive, with in-flight feedback; on success the
    /// next poll drops the mounted volume (and the detail view empties).
    func ejectDisc(_ drive: OpticalDrive) {
        guard !ejectingDiscs.contains(drive.id) else { return }
        ejectingDiscs.insert(drive.id)
        DiscService.eject(driveId: drive.id) { ok in
            self.ejectingDiscs.remove(drive.id)
            if ok {
                self.discTracks = []
                self.pollDiscDrives()
            } else {
                self.discStatus = "Couldn't eject the disc"
            }
        }
    }
}
