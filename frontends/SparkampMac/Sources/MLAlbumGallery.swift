import SwiftUI
import AppKit

// MARK: - Album gallery (Phase 11 A4, nav = .albums)
//
// Grid of cover-art tiles, one per (album, effective album-artist) group —
// the core (`MediaLibrary::albums` via `sparkamp_ml_albums`) does the
// grouping so phase 10's `effective_album_artist` toggle stays the single
// source of truth; mac never re-derives it. Tapping a tile sets
// `model.mlSelectedAlbum` and switches to the Files view, which honors that
// filter by loading `albumTracks(...)` instead of the full library (see
// `MediaLibraryView.reload()`) — mirrors GTK's `album_filter`. "Play Album" /
// "Enqueue Album" route through the exact same `mlReplacePlaylistWith` /
// `mlAddToPlaylist` calls the playlist editor's whole-playlist Play/Enqueue
// buttons already use (`MLPlaylistEditor.swift`) — no duplicated playback
// logic.
//
// `LazyVGrid` only realizes on-screen cells, so a per-cell async cover load
// (mirroring `DeviceConflictSheet.swift`'s `ConflictArtworkThumb`) is the
// whole story for keeping a large library scrolling smoothly — there's no
// separate view-recycling layer to build here, unlike GTK's `GridView`.
//
// Zoom + sort are persisted with `@AppStorage`, the same idiom as
// `sparkamp.ml.sidebarWidth` in `MediaLibraryWindow.swift`. GTK persists the
// equivalent `gallery_thumb_px` / `gallery_sort` fields in its TOML config
// instead — this divergence (mac window prefs live in AppStorage, not the
// shared config file) is intentional and called out in
// `docs/mac-pass-checklist.md`.
struct MLAlbumGallery: View {
    @Binding var nav: MLNavigation
    /// Live text from the Media Library toolbar's search field, which renders
    /// in the same leading slot for this view as it does for Files. Filtering
    /// happens here rather than in the core: the whole album list is already
    /// in memory and a substring match over a few thousand rows is far cheaper
    /// than a round trip through `sparkamp_ml_albums` per keystroke.
    let searchQuery: String
    let theme: SkinTheme

    @EnvironmentObject var model: SparkampModel

    /// Cell edge length in points; zoom `Slider` range 96...256.
    @AppStorage("sparkamp.gallery.thumbPx") private var thumbPx: Double = 160
    /// 0 = Artist, 1 = Album, 2 = Year — passed straight through to
    /// `sparkamp_ml_album_count`/`sparkamp_ml_albums`'s `sort` parameter.
    @AppStorage("sparkamp.gallery.sort") private var sortIndex: Int = 0

    @State private var albums: [AlbumGroup] = []

    /// True briefly after a zoom change while the grid re-lays out and the
    /// newly-revealed cells load their covers — drives the "please wait"
    /// spinner (mirrors GTK's zoom spinner).
    @State private var zooming = false

    private let zoomMin: Double = 96
    private let zoomMax: Double = 256
    private let zoomStep: Double = 32

    private var columns: [GridItem] {
        [GridItem(.adaptive(minimum: CGFloat(thumbPx), maximum: CGFloat(thumbPx) * 1.35), spacing: 16)]
    }

    /// Albums matching the toolbar query. Matched against the *displayed*
    /// title and artist, not the raw fields, so "(no album)" and "Unknown
    /// Artist" find the tiles that show those words — a user searching for
    /// what they can read on screen is the case worth serving.
    private var visibleAlbums: [AlbumGroup] {
        let q = searchQuery.trimmingCharacters(in: .whitespaces)
        guard !q.isEmpty else { return albums }
        return albums.filter {
            $0.displayAlbum.localizedCaseInsensitiveContains(q)
                || $0.displayArtist.localizedCaseInsensitiveContains(q)
        }
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider().background(theme.windowBorder)
            if visibleAlbums.isEmpty {
                Spacer()
                Text(emptyMessage)
                    .multilineTextAlignment(.center)
                    .font(theme.vars.bodyFont)
                    .foregroundStyle(theme.playlistDurationText)
                Spacer()
            } else {
                ScrollView {
                    LazyVGrid(columns: columns, spacing: 20) {
                        ForEach(visibleAlbums) { album in
                            AlbumCell(
                                album: album,
                                thumbPx: thumbPx,
                                theme: theme,
                                onActivate: { activate(album) },
                                onPlay: { play(album) },
                                onEnqueue: { enqueue(album) },
                                onDrag: { dragPayload(album) }
                            )
                        }
                    }
                    .padding(16)
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(theme.background)
        .onAppear { reloadAlbums() }
        .onChange(of: sortIndex) { _, _ in reloadAlbums() }
        // A DB write elsewhere (rescan, tag edit, artwork refresh) can change
        // grouping/artwork — reuse the same reload triggers the Files view
        // already listens to (MediaLibraryWindow.swift's onChange(of:
        // mlReloadTrigger)/onChange(of: mlScanRunning)).
        .onChange(of: model.mlReloadTrigger) { _, _ in reloadAlbums() }
        .onChange(of: model.mlScanRunning) { _, running in if !running { reloadAlbums() } }
    }

    /// Distinguishes "your library has no albums" from "your query matched
    /// none of them" — the fix for the first is adding a folder, for the
    /// second it's clearing the search.
    private var emptyMessage: String {
        albums.isEmpty
            ? "No albums yet.\nAdd a folder to your library to see albums here."
            : "No albums match your search."
    }

    private var header: some View {
        HStack(spacing: 12) {
            Text("Albums")
                .font(theme.vars.bodyFont.weight(.semibold))
                .foregroundStyle(theme.playlistDurationText)
            if !visibleAlbums.isEmpty {
                // `verbatim:` — a bare "\(count)" would go through
                // LocalizedStringKey and pick up the locale's grouping
                // separator, printing a 1,234-album library as "1,234" while
                // every other count in this window ("278 tracks") stays plain.
                Text(verbatim: "\(visibleAlbums.count)")
                    .font(.system(size: 10))
                    .foregroundStyle(theme.playlistDurationText)
            }

            Spacer()

            Picker("Sort", selection: $sortIndex) {
                Text("Artist").tag(0)
                Text("Album").tag(1)
                Text("Year").tag(2)
            }
            .labelsHidden()
            .frame(width: 130)

            // "Please wait" while a zoom change re-lays out the grid and the
            // newly-visible cells load their covers.
            if zooming {
                ProgressView()
                    .controlSize(.small)
                    .help("Rendering new size…")
            }

            // Zoom as −/＋ buttons with a plain "Zoom" label between them
            // (no pixel size shown) — matches the GTK gallery.
            HStack(spacing: 6) {
                Button { changeZoom(by: -zoomStep) } label: {
                    Image(systemName: "minus")
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(thumbPx <= zoomMin)

                Text("Zoom")
                    .font(theme.vars.bodyFont)
                    .foregroundStyle(theme.playlistDurationText)

                Button { changeZoom(by: zoomStep) } label: {
                    Image(systemName: "plus")
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(thumbPx >= zoomMax)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(theme.background)
    }

    /// Step the thumbnail size by `delta`, clamped to `zoomMin...zoomMax`, and
    /// show the please-wait spinner briefly while the grid settles. `@AppStorage`
    /// persists the new size, same as before.
    private func changeZoom(by delta: Double) {
        let newValue = min(max(thumbPx + delta, zoomMin), zoomMax)
        guard newValue != thumbPx else { return }
        zooming = true
        thumbPx = newValue
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.35) {
            zooming = false
        }
    }

    private func reloadAlbums() {
        albums = model.loadAlbums(sort: sortIndex)
    }

    /// Tap → filter the Files table to this album's tracks, mirroring GTK's
    /// `on_album_activate` (set the filter, then switch to Files).
    private func activate(_ album: AlbumGroup) {
        model.mlSelectedAlbum = AlbumFilter(album: album.album, albumArtist: album.albumArtist)
        nav = .files
    }

    /// "Play Album" — replaces the active playlist; the same call the
    /// playlist editor's whole-playlist "Play" button makes.
    private func play(_ album: AlbumGroup) {
        let ids = model.albumTracks(album: album.album, albumArtist: album.albumArtist).map(\.id)
        guard !ids.isEmpty else { return }
        model.mlReplacePlaylistWith(ids: ids)
    }

    /// "Enqueue Album" — appends to the active playlist; the same call the
    /// playlist editor's whole-playlist "Enqueue" button makes.
    private func enqueue(_ album: AlbumGroup) {
        let ids = model.albumTracks(album: album.album, albumArtist: album.albumArtist).map(\.id)
        guard !ids.isEmpty else { return }
        model.mlAddToPlaylist(ids: ids)
    }

    /// Dragging a tile drags the album — GTK's container rule, the same one
    /// that makes a drive card stand for its disc and a device card for its
    /// files.
    ///
    /// Carries library ids, so the drop adds the tracks from the library's
    /// own records rather than re-reading tags off every file. The file paths
    /// ride along for a drop outside Sparkamp, which has no idea what a
    /// library id is.
    private func dragPayload(_ album: AlbumGroup) -> NSItemProvider {
        let tracks = model.albumTracks(album: album.album, albumArtist: album.albumArtist)
        return SparkampDrag.begin(.libraryIds(tracks.map(\.id)),
                                  pasteboardPaths: tracks.map(\.path))
    }
}

// MARK: - Album cell

private struct AlbumCell: View {
    let album: AlbumGroup
    let thumbPx: Double
    let theme: SkinTheme
    let onActivate: () -> Void
    let onPlay: () -> Void
    let onEnqueue: () -> Void
    /// Built by the gallery, which is the view that can reach the library.
    let onDrag: () -> NSItemProvider

    @State private var image: NSImage? = nil
    @State private var loaded = false

    var body: some View {
        Button(action: onActivate) {
            VStack(alignment: .leading, spacing: 6) {
                cover
                Text(displayAlbum)
                    .font(theme.vars.bodyFont.weight(.medium))
                    .foregroundStyle(theme.playlistText)
                    .lineLimit(1)
                    .truncationMode(.tail)
                HStack(spacing: 4) {
                    Text(displayArtist)
                        .font(.system(size: 11))
                        .foregroundStyle(theme.playlistDurationText)
                        .lineLimit(1)
                        .truncationMode(.tail)
                    if let year = album.year {
                        // `verbatim:` is load-bearing: the LocalizedStringKey
                        // overload formats an interpolated integer through the
                        // current locale, which turned every release year into
                        // "2,014".
                        Text(verbatim: "· \(year)")
                            .font(.system(size: 11))
                            .foregroundStyle(theme.playlistDurationText)
                    }
                }
            }
            .frame(width: CGFloat(thumbPx), alignment: .leading)
        }
        .buttonStyle(.plain)
        .contextMenu {
            Button("Play Album") { onPlay() }
            Button("Enqueue Album") { onEnqueue() }
        }
        .onDrag(onDrag)
        .help("\(displayArtist) — \(displayAlbum)")
        .onAppear(perform: loadArt)
    }

    private var displayAlbum: String { album.displayAlbum }
    private var displayArtist: String { album.displayArtist }

    @ViewBuilder
    private var cover: some View {
        Group {
            if let img = image {
                Image(nsImage: img)
                    .resizable()
                    .aspectRatio(contentMode: .fill)
            } else {
                placeholder
            }
        }
        .frame(width: CGFloat(thumbPx), height: CGFloat(thumbPx))
        .clipShape(RoundedRectangle(cornerRadius: 6))
        .overlay(RoundedRectangle(cornerRadius: 6).stroke(theme.windowBorder, lineWidth: 1))
        .overlay(alignment: .bottomTrailing) { trackCountBadge }
    }

    /// How many of this album's tracks are in the library, as a small pill in
    /// the bottom-right of the cover. Sits on its own translucent backing so
    /// it stays legible over both a bright cover and the dark placeholder.
    private var trackCountBadge: some View {
        Text(verbatim: "\(album.trackCount)")
            .font(.system(size: 10, weight: .semibold))
            .foregroundStyle(.white)
            .padding(.horizontal, 5)
            .padding(.vertical, 2)
            .background(
                Capsule().fill(Color.black.opacity(0.65))
            )
            .padding(5)
    }

    /// Same "no artwork" treatment as the A6 artwork window / A1 panel
    /// (`ArtworkWindow.swift`): a 50%-opacity app icon, scaled to the tile.
    private var placeholder: some View {
        ZStack {
            theme.lcdBackground.opacity(0.5)
            Image(nsImage: NSApp.applicationIconImage)
                .resizable()
                .frame(width: CGFloat(thumbPx) * 0.4, height: CGFloat(thumbPx) * 0.4)
                .opacity(0.5)
        }
    }

    /// Loads the representative cover off the main thread — same
    /// background-then-main-actor idiom as `ConflictArtworkThumb` in
    /// `DeviceConflictSheet.swift`. The gallery can hold thousands of
    /// albums; `LazyVGrid` only instantiates on-screen cells, so this only
    /// ever runs for tiles the user actually scrolls to.
    private func loadArt() {
        guard !loaded, !album.artworkPath.isEmpty else { return }
        loaded = true
        let path = album.artworkPath
        DispatchQueue.global(qos: .userInitiated).async {
            let img = NSImage(contentsOfFile: path)
            DispatchQueue.main.async { self.image = img }
        }
    }
}
