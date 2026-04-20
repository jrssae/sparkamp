import SwiftUI

// MARK: - Deduplication Window

struct DeduplicatorView: View {
    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager
    @Environment(\.dismiss) private var dismiss

    @State private var showCancelAlert = false
    @State private var expandedGroups: Set<UUID> = []

    private var probableGroups: [DedupGroupItem] {
        model.dedupGroups.filter { $0.confidence == 0 }
    }
    private var lessLikelyGroups: [DedupGroupItem] {
        model.dedupGroups.filter { $0.confidence != 0 }
    }

    var body: some View {
        let vars = themeManager.currentVars
        return VStack(spacing: 0) {
            // ── Header ────────────────────────────────────────────────────────
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Deduplication")
                        .font(.headline)
                    if model.dedupRunning {
                        Text("Scanning… \(model.dedupGroups.count) groups found so far")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    } else if !model.dedupGroups.isEmpty {
                        Text("\(model.dedupGroups.count) duplicate groups found")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    } else if !model.dedupRunning {
                        Text("Click Scan to find duplicate tracks")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                Spacer()
                if model.dedupRunning {
                    ProgressView()
                        .controlSize(.small)
                        .padding(.trailing, 4)
                    Button("Cancel") {
                        showCancelAlert = true
                    }
                    .buttonStyle(.bordered)
                    .foregroundStyle(.red)
                } else {
                    Button(model.dedupGroups.isEmpty ? "Scan" : "Rescan") {
                        model.freeDedup()
                        model.startDedup()
                    }
                    .buttonStyle(.borderedProminent)
                }
            }
            .padding()

            if model.dedupRunning && model.dedupGroups.isEmpty {
                Divider()
                HStack {
                    ProgressView()
                        .controlSize(.small)
                    Text("Analyzing library for duplicates…")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                .padding()
                Spacer()
            } else if model.dedupGroups.isEmpty {
                Divider()
                Spacer()
                Image(systemName: "doc.on.doc")
                    .font(.system(size: 40))
                    .foregroundStyle(.secondary)
                    .padding(.bottom, 8)
                Text("No duplicates found")
                    .foregroundStyle(.secondary)
                Text("Scan your media library to find duplicate tracks.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Spacer()
            } else {
                Divider()
                // ── Results list ──────────────────────────────────────────────
                List {
                    if !probableGroups.isEmpty {
                        Section {
                            ForEach(probableGroups) { group in
                                GroupRow(group: group,
                                         isExpanded: expandedGroups.contains(group.id),
                                         onToggle: { toggle(group.id) })
                            }
                        } header: {
                            HStack {
                                Label("Probable (\(probableGroups.count) groups)",
                                      systemImage: "exclamationmark.triangle.fill")
                                    .foregroundStyle(.orange)
                                    .font(vars.bodyFont.weight(.semibold))
                            }
                        }
                    }

                    if !lessLikelyGroups.isEmpty {
                        Section {
                            ForEach(lessLikelyGroups) { group in
                                GroupRow(group: group,
                                         isExpanded: expandedGroups.contains(group.id),
                                         onToggle: { toggle(group.id) })
                            }
                        } header: {
                            HStack {
                                Label("Less Likely (\(lessLikelyGroups.count) groups)",
                                      systemImage: "questionmark.circle")
                                    .foregroundStyle(.secondary)
                                    .font(vars.bodyFont.weight(.semibold))
                            }
                        }
                    }
                }
                .listStyle(.inset)
            }
        }
        .frame(minWidth: 560, minHeight: 400)
        .alert("Cancel Scan?", isPresented: $showCancelAlert) {
            Button("Keep Scanning", role: .cancel) {}
            Button("Cancel Scan", role: .destructive) {
                model.cancelDedup()
            }
        } message: {
            Text("Partial results will be shown.")
        }
        .onDisappear {
            model.dedupVisible = false
            if model.dedupRunning { model.cancelDedup() }
        }
    }

    private func toggle(_ id: UUID) {
        if expandedGroups.contains(id) { expandedGroups.remove(id) }
        else { expandedGroups.insert(id) }
    }
}

// MARK: - Group row

private struct GroupRow: View {
    let group: DedupGroupItem
    let isExpanded: Bool
    let onToggle: () -> Void

    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager

    var body: some View {
        let vars = themeManager.currentVars
        return VStack(alignment: .leading, spacing: 0) {
            // Group header
            HStack(spacing: 6) {
                Image(systemName: isExpanded ? "chevron.down" : "chevron.right")
                    .font(.system(size: 9))
                    .foregroundStyle(.secondary)
                    .frame(width: 12)

                Text(group.label)
                    .font(vars.bodyFont.weight(.medium))
                    .lineLimit(1)

                Text("(\(group.tracks.count) tracks)")
                    .font(vars.bodyFont)
                    .foregroundStyle(.secondary)

                Spacer()

                // Confidence badge
                Text(group.confidenceLabel)
                    .font(vars.bodyFont.weight(.semibold))
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(group.confidence == 0
                                ? Color.orange.opacity(0.2)
                                : Color.secondary.opacity(0.15))
                    .foregroundStyle(group.confidence == 0 ? .orange : .secondary)
                    .cornerRadius(4)
            }
            .padding(.vertical, 4)
            .contentShape(Rectangle())
            .onTapGesture { onToggle() }
            .contextMenu {
                Button("Add to Playlist") { model.dedupAddGroupToPlaylist(group) }
                Button("Replace Playlist") { model.dedupReplacePlaylistWithGroup(group) }
            }

            // Track children
            if isExpanded {
                ForEach(group.tracks) { track in
                    TrackRow(track: track)
                        .padding(.leading, 20)
                }
            }
        }
    }
}

// MARK: - Track row inside a group

private struct TrackRow: View {
    let track: DedupTrackItem
    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager

    var body: some View {
        let vars = themeManager.currentVars
        return HStack(spacing: 6) {
            Image(systemName: "music.note")
                .font(.system(size: 9))
                .foregroundStyle(.secondary)
                .frame(width: 12)

            VStack(alignment: .leading, spacing: 1) {
                Text(track.title.isEmpty ? track.filename : track.title)
                    .font(vars.bodyFont)
                    .lineLimit(1)
                Text(track.path)
                    .font(vars.bodyFont)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }

            Spacer()

            Text(track.durationString)
                .font(vars.smallMonospaceFont)
                .monospacedDigit()
                .foregroundStyle(.secondary)
        }
        .padding(.vertical, 2)
        .contextMenu {
            Button("Open in Finder") { model.openInFinder(track.path) }
        }
    }
}
