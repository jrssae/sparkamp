import Foundation
import AppKit

// MARK: - Optical-disc operations
//
// State lives in SparkampModel (extensions can't hold stored properties).
// Detection and track listing shell out to drutil/plutil inside the core, so
// both always run on a background queue and hop back to the main actor —
// mirroring the device-sync threading model.

extension SparkampModel {

    /// Re-enumerate optical drives (background) and publish changes. When a
    /// drive transitions to "audio CD loaded" and the auto-open setting is on,
    /// bring the Media Library to that drive (default-CD-handler flow).
    func pollDiscDrives() {
        DispatchQueue.global(qos: .utility).async {
            let drives = DiscService.listDrives()
            DispatchQueue.main.async {
                // A drive counts as freshly inserted when it holds an audio CD
                // now but didn't in the prior snapshot. On the first poll the
                // prior list is empty, so a disc present at launch also counts
                // (accepted — an already-loaded CD reads as freshly inserted).
                let wasAudio: (String) -> Bool = { id in
                    self.discDrives.first(where: { $0.id == id })?.media.isAudioCd ?? false
                }
                let inserted = drives.first(where: { $0.media.isAudioCd && !wasAudio($0.id) })

                if drives != self.discDrives {
                    self.discDrives = drives
                    self.pruneBurnQueues()
                }

                if let drive = inserted, self.autoShowInsertedCd {
                    self.autoOpenLibrary(to: drive.id)
                }
            }
        }
    }

    /// Whether inserting an audio CD should auto-open the library (default true).
    private var autoShowInsertedCd: Bool {
        guard let ctx = ctx else { return true }
        return sparkamp_get_auto_show_inserted_cd(ctx)
    }

    /// Foreground the Media Library and request navigation to a drive. The nav
    /// request is set first so an already-open library reacts via its onChange;
    /// opening the window (if closed) lets its onAppear consume the request.
    func autoOpenLibrary(to driveId: String) {
        requestedDiscNav = driveId
        openMediaLibrary()
        NSApp.activate(ignoringOtherApps: true)
    }

    /// Load the playlist-ready track entries for one drive's disc into
    /// `discTracks` (background; empty when no audio disc), restore the
    /// disc's persisted tag record on first sight (the on-disk cache is what
    /// makes names survive an app restart), then overlay the titles.
    func loadDiscTracks(_ drive: OpticalDrive) {
        discBusy = true
        DispatchQueue.global(qos: .userInitiated).async {
            let entries = DiscService.trackEntries(drive: drive)
            // Same background block: discId is pure math, tagsGet is file IO.
            let discId = drive.toc.flatMap { DiscService.discId(toc: $0) }
            let stored = discId.map { DiscService.tagsGet(discid: $0) }
            DispatchQueue.main.async {
                self.discTracks = entries
                self.discBusy = false
                if let id = discId, self.discTagSets[id] == nil,
                   let user = stored?.user {
                    self.discTagSets[id] = DiscTagSet(
                        artist: user.artist,
                        album: user.album,
                        year: user.year,
                        genre: user.genre,
                        titles: user.trackTitles)
                    if let official = stored?.official {
                        self.discOfficial[id] = official
                    }
                }
                self.applyDiscTagTitles(drive)
            }
        }
    }

    /// The freedb id of the drive's loaded disc, or nil without an audio disc.
    func discIdFor(_ drive: OpticalDrive) -> String? {
        drive.toc.flatMap { DiscService.discId(toc: $0) }
    }

    /// The configured gnudb email ("" when unset) — one accessor for the
    /// identify/choose/submit paths.
    func gnudbEmail() -> String {
        guard let ctx = ctx else { return "" }
        let p = sparkamp_get_gnudb_email(ctx)
        defer { sparkamp_free_string(p) }
        return p.map { String(cString: $0) } ?? ""
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

    /// Final playlist metadata for one disc entry under a tag set: the
    /// overlaid title (xmcd sampler "Artist / Title" split into a per-track
    /// artist), the disc artist as fallback, and the album. One source of
    /// truth for adding rows AND propagating edits into existing ones.
    private func discEntryMeta(
        _ entry: DiscTrackEntry, tags: DiscTagSet?
    ) -> (title: String, artist: String, album: String) {
        var title = entry.title
        var artist = tags?.artist ?? ""
        if let range = title.range(of: " / ") {
            artist = String(title[..<range.lowerBound])
            title = String(title[range.upperBound...])
        }
        return (title, artist, tags?.album ?? "")
    }

    /// Store an edited tag set for the drive's disc, refresh the titles,
    /// persist the record (survives restarts), and push the new metadata
    /// into every already-added active-playlist row for this disc.
    func saveDiscTags(_ drive: OpticalDrive, tags: DiscTagSet) {
        guard let id = discIdFor(drive) else { return }
        discTagSets[id] = tags
        applyDiscTagTitles(drive)
        discStatus = "Tags saved for this disc"

        // Immediate propagation: disc entries and playlist rows share exact
        // path strings, so update matching rows in place.
        if let ctx = ctx, !discTracks.isEmpty {
            var touched = 0
            for entry in discTracks {
                let meta = discEntryMeta(entry, tags: tags)
                touched += Int(entry.path.withCString { p in
                    meta.title.withCString { t in
                        meta.artist.withCString { a in
                            meta.album.withCString { al in
                                sparkamp_playlist_update_entry_meta(ctx, p, t, a, al)
                            }
                        }
                    }
                })
            }
            if touched > 0 {
                refreshPlaylist()
                refreshCurrentTrackInfo()
            }
        }
        let user = XmcdEntry(
            discid: id,
            artist: tags.artist,
            album: tags.album,
            year: tags.year,
            genre: tags.genre,
            trackTitles: tags.titles,
            extd: "",
            extt: [],
            revision: 0)
        let official = discOfficial[id]
        DispatchQueue.global(qos: .utility).async {
            DiscService.tagsSet(discid: id, user: user, official: official)
        }
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
        guard let toc = drive.toc, !discIdentifying, ctx != nil else { return }
        let email = gnudbEmail()
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
                        // Tag the matches with their drive: the lookup keeps
                        // running if the window closed or the user navigated
                        // away, and the picker re-presents on that drive only.
                        self.discMatchesDriveId = drive.id
                        self.discMatches = matches
                        self.discStatus =
                            "\(matches.count) gnudb candidates — pick one"
                    }
                }
            }
        }
    }

    /// User picked a match from the sheet (or the single exact match).
    func chooseDiscMatch(_ drive: OpticalDrive, match: DiscMatch) {
        guard ctx != nil else { return }
        let email = gnudbEmail()
        discMatches = nil
        discMatchesDriveId = nil
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
        let email = gnudbEmail()
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

    /// Add disc tracks to the active playlist with their tags: title from the
    /// entry (already overlaid with the disc's tag set), artist/album from
    /// the disc-level tags so the playlist shows "Artist - Title" like every
    /// other entry. A title in the xmcd sampler convention ("Artist / Title")
    /// splits into a per-track artist. No metadata scan or duration probe:
    /// the AIFFs carry no tags and the durations are already exact.
    ///
    /// Mirrors `mlDoubleClickTracks` semantics: honors the replace/append
    /// add-behavior setting, and autoplay-on-add starts the first new track
    /// when the playlist was replaced or was empty (never interrupts a track
    /// already playing).
    func addDiscTracks(_ drive: OpticalDrive, entries: [DiscTrackEntry]) {
        guard let ctx = ctx, !entries.isEmpty else { return }
        let tags = discIdFor(drive).flatMap { discTagSets[$0] }
        let shouldReplace = Int(sparkamp_get_playlist_add_behavior(ctx)) == 1
        let autoplay = sparkamp_get_autoplay_on_add(ctx)
        if shouldReplace { clearPlaylist() }
        let indexBefore = Int(sparkamp_playlist_len(ctx))
        let wasEmpty = indexBefore == 0
        for e in entries {
            let meta = discEntryMeta(e, tags: tags)
            e.path.withCString { p in
                meta.title.withCString { t in
                    meta.artist.withCString { a in
                        meta.album.withCString { al in
                            _ = sparkamp_playlist_add_entry(
                                ctx, p, t, a, al, Int32(e.durationSecs))
                        }
                    }
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

    // MARK: Rip

    /// Rip the given tracks through the core's job runner (the same loop
    /// GTK/TUI use): destination write pre-flight (a read-only folder fails
    /// once, clearly, before touching the drive), per-track tagging, cancel
    /// between tracks, and within-track progress (`ripTrackFrac`). Finished
    /// files are auto-imported; the result line is the shared core message.
    func ripDiscTracks(
        _ drive: OpticalDrive, entries: [DiscTrackEntry], destRoot: String, quality: Int
    ) {
        guard !entries.isEmpty, ripProgress == nil else { return }
        let discid = discIdFor(drive) ?? ""
        let tagSet = discTagSets[discid]
        let tags = XmcdEntry(
            discid: discid,
            artist: tagSet?.artist ?? "",
            album: tagSet?.album ?? "",
            year: tagSet?.year ?? "",
            genre: tagSet?.genre ?? "",
            trackTitles: tagSet?.titles ?? [],
            extd: "",
            extt: [],
            revision: 0)
        let total = drive.toc?.tracks.count ?? entries.count

        // Remember the destination for next time.
        if let ctx = ctx {
            destRoot.withCString { sparkamp_set_rip_dest(ctx, $0) }
            sparkamp_set_rip_quality(ctx, Int32(quality))
        }

        let job = DiscService.RipRunJob(
            entries: entries,
            destRoot: destRoot,
            quality: quality,
            tags: tags,
            totalOnDisc: total)
        guard DiscService.ripJobStart(job: job) else {
            discStatus = "Couldn't start the rip (is another rip running?)"
            return
        }
        ripCancelRequested = false
        ripTrackFrac = 0
        ripProgress = CopyProgress(done: 0, total: entries.count, name: entries[0].title)
        discStatus = nil

        // Poll the core job from the main run loop; `done` ends the timer.
        var cancelSent = false
        Timer.scheduledTimer(withTimeInterval: 0.3, repeats: true) { [weak self] timer in
            guard let self else {
                timer.invalidate()
                return
            }
            if self.ripCancelRequested && !cancelSent {
                DiscService.ripJobCancel()
                cancelSent = true
            }
            guard let st = DiscService.ripJobPoll() else { return }
            if let done = st.done {
                timer.invalidate()
                self.ripProgress = nil
                self.ripTrackFrac = 0
                self.ripCancelRequested = false
                // Import only registers files under watched folders; the
                // shared message reports honestly either way.
                var imported = 0
                if !done.ripped.isEmpty {
                    imported = self.mlAddFilesToLibrary(paths: done.ripped)
                }
                self.discStatus = DiscService.ripResultMessage(done: done, imported: imported)
            } else if st.running {
                self.ripProgress = CopyProgress(
                    done: st.trackIndex, total: st.trackCount, name: st.title)
                self.ripTrackFrac = st.frac
            }
        }
    }

    // MARK: Burn queues (per-drive) + burning (blind-implemented; hardware pass = Opus)

    /// The burn queue for one drive — empty when nothing has been queued
    /// there yet. Always read through this accessor rather than indexing
    /// `burnQueues` directly.
    func burnQueue(for driveId: String) -> [BurnEntry] {
        burnQueues[driveId] ?? []
    }

    func burnListTotalSecs(for driveId: String) -> Int {
        burnQueue(for: driveId).reduce(0) { $0 + ($1.durationSecs ?? 0) }
    }

    func burnListTotalBytes(for driveId: String) -> UInt64 {
        burnQueue(for: driveId).reduce(0) { $0 + $1.bytes }
    }

    func removeFromBurnList(driveId: String, at offsets: IndexSet) {
        burnQueues[driveId, default: []].remove(atOffsets: offsets)
    }

    func clearBurnList(driveId: String) {
        burnQueues[driveId] = []
    }

    /// Drop queues for drives no longer attached — they'd otherwise linger
    /// invisibly (no panel shows them). Mirrors the core's
    /// `BurnQueues::remove_gone`; call after every drive-list poll.
    private func pruneBurnQueues() {
        let live = Set(discDrives.map(\.id))
        burnQueues = burnQueues.filter { live.contains($0.key) }
    }

    /// Queue files for burning onto ONE drive's queue (dedup by path within
    /// that drive only — the whole point of per-drive queues is that
    /// "Send to ▸ Disc Drive → B" never touches any other drive's list).
    /// Duration comes from the caller's library metadata when known
    /// (`durations`); anything unknown is probed off the main thread via
    /// `sparkamp_disc_probe_durations`. A path that comes back unreadable
    /// (no library duration AND a null probe result) is never queued — an
    /// unknown duration would defeat the over-capacity gate — instead it's
    /// collected into `burnUnreadableFiles` for a single alert. Mirrors the
    /// core's `disc::burnlist::add_files` + GTK's `show_unreadable_dialog`.
    func addToBurnList(
        driveId: String, driveLabel: String,
        paths: [String], displays: [String: String], durations: [String: Int]
    ) {
        guard !paths.isEmpty else { return }
        let already = Set(burnQueue(for: driveId).map(\.path))
        let candidates = paths.filter { !already.contains($0) }
        let duplicateCount = paths.count - candidates.count
        guard !candidates.isEmpty else {
            discStatus = "Already queued on \(driveLabel)"
            return
        }

        func finish(_ probed: [String: UInt32?]) {
            var secs = durations
            for p in candidates where secs[p] == nil {
                if let s = probed[p] ?? nil { secs[p] = Int(s) }
            }
            let unreadable = candidates.filter { secs[$0] == nil }
            var added = 0
            for p in candidates where secs[p] != nil {
                let bytes = (try? FileManager.default.attributesOfItem(atPath: p)[.size] as? UInt64)
                    .flatMap { $0 } ?? 0
                self.burnQueues[driveId, default: []].append(BurnEntry(
                    path: p,
                    display: displays[p] ?? URL(fileURLWithPath: p).lastPathComponent,
                    durationSecs: secs[p],
                    bytes: bytes))
                added += 1
            }
            let total = self.burnQueue(for: driveId).count
            var msg = "Queued \(added) for burning on \(driveLabel) (\(total) on the list)"
            if duplicateCount > 0 { msg += " — \(duplicateCount) already queued" }
            self.discStatus = msg
            if !unreadable.isEmpty { self.burnUnreadableFiles = unreadable }
        }

        let needsProbe = candidates.filter { durations[$0] == nil }
        guard !needsProbe.isEmpty else { finish([:]); return }
        discStatus = "Reading files…"
        probeDurations(paths: needsProbe, completion: finish)
    }

    /// Resolve display/duration metadata from the library where possible
    /// (falls back to a filename display + a full probe when the path isn't
    /// a library row — e.g. a device file), then queue onto one drive's burn
    /// list. The single entry point every "Send to ▸ Disc Drive" action
    /// calls (SendToMenu, DeviceMenu.swift's NSMenu builder).
    func sendPathsToDrive(_ driveId: String, paths: [String]) {
        guard !paths.isEmpty else { return }
        let driveLabel = discDrives.first(where: { $0.id == driveId })?.label ?? driveId
        var displays: [String: String] = [:]
        var durations: [String: Int] = [:]
        for p in paths {
            guard let t = mlTracks.first(where: { $0.path == p }) else { continue }
            displays[p] = t.artist.isEmpty ? t.title : "\(t.artist) - \(t.title)"
            if t.lengthSecs > 0 { durations[p] = Int(t.lengthSecs) }
        }
        addToBurnList(driveId: driveId, driveLabel: driveLabel,
                      paths: paths, displays: displays, durations: durations)
    }

    /// Probe durations for paths off the main thread (GStreamer discovery
    /// per file can take real time) — decode + free follow the same
    /// `takeString`/`decoder()` pattern as every other `DiscService` call;
    /// `completion` runs back on the main actor. Nil map on FFI failure.
    func probeDurations(paths: [String], completion: @escaping ([String: UInt32?]) -> Void) {
        guard !paths.isEmpty else {
            DispatchQueue.main.async { completion([:]) }
            return
        }
        DispatchQueue.global(qos: .userInitiated).async {
            let map = DiscService.probeDurations(paths: paths)
            DispatchQueue.main.async { completion(map) }
        }
    }

    /// Burn the queue as an audio CD (erase first when the user confirmed it,
    /// RW with content). The whole orchestration — staging, erase, per-track
    /// WAV preparation, the burn, cleanup — is the shared core job (the same
    /// one GTK/TUI run); this side only starts it and mirrors its phase line.
    func burnAudio(_ drive: OpticalDrive, eraseFirst: Bool) {
        startBurnJob(drive, audio: true, eraseFirst: eraseFirst)
    }

    /// Burn the queue as a data disc (files at the disc root + the MP3-CD
    /// companion playlist; staging and name-dedup happen in the core job).
    func burnData(_ drive: OpticalDrive, eraseFirst: Bool) {
        startBurnJob(drive, audio: false, eraseFirst: eraseFirst)
    }

    private func startBurnJob(_ drive: OpticalDrive, audio: Bool, eraseFirst: Bool) {
        let items = burnQueue(for: drive.id)
        guard !items.isEmpty, burnPhase == nil, let ctx = ctx else { return }
        let job = DiscService.BurnRunJob(
            drive: drive,
            items: items.map { DiscService.BurnJobItem(path: $0.path, display: $0.display) },
            audio: audio,
            // The companion playlist follows the app-wide format setting.
            useM3u: sparkamp_get_playlist_format(ctx) == 1,
            eraseFirst: eraseFirst,
            verify: sparkamp_get_burn_verify(ctx))
        guard DiscService.burnJobStart(job: job) else {
            discStatus = "Couldn't start the burn (is another burn running?)"
            return
        }
        burnPhase = "Starting…"
        discStatus = nil
        let driveId = drive.id

        // Poll the core job from the main run loop; `done` ends the timer.
        Timer.scheduledTimer(withTimeInterval: 0.3, repeats: true) { [weak self] timer in
            guard let self else {
                timer.invalidate()
                return
            }
            guard let st = DiscService.burnJobPoll() else { return }
            if let done = st.done {
                timer.invalidate()
                self.burnPhase = nil
                if done.ok {
                    self.discStatus = done.message
                    self.clearBurnList(driveId: driveId)
                    self.pollDiscDrives()
                } else if done.message == "cancelled" {
                    self.discStatus = "Burn cancelled"
                } else {
                    self.discStatus = "Burn failed: \(done.message)"
                }
            } else if st.running {
                self.burnPhase = st.phase
            }
        }
    }

    /// Cancel the burn: the core job stops between steps and kills any live
    /// erase/burn subprocess.
    func cancelBurn() {
        DiscService.burnCancel()
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
