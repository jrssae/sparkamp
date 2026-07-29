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
    let theme: SkinTheme

    @EnvironmentObject var model: SparkampModel

    /// Cell edge length in points; zoom `Slider` range 96...256.
    @AppStorage("sparkamp.gallery.thumbPx") private var thumbPx: Double = 160
    /// 0 = Artist, 1 = Album, 2 = Year — passed straight through to
    /// `sparkamp_ml_album_count`/`sparkamp_ml_albums`'s `sort` parameter.
    @AppStorage("sparkamp.gallery.sort") private var sortIndex: Int = 0

    @State private var albums: [AlbumGroup] = []

    private var columns: [GridItem] {
        [GridItem(.adaptive(minimum: CGFloat(thumbPx), maximum: CGFloat(thumbPx) * 1.35), spacing: 16)]
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider().background(theme.windowBorder)
            if albums.isEmpty {
                Spacer()
                Text("No albums yet.\nAdd a folder to your library to see albums here.")
                    .multilineTextAlignment(.center)
                    .font(theme.vars.bodyFont)
                    .foregroundStyle(theme.playlistDurationText)
                Spacer()
            } else {
                ScrollView {
                    LazyVGrid(columns: columns, spacing: 20) {
                        ForEach(albums) { album in
                            AlbumCell(
                                album: album,
                                thumbPx: thumbPx,
                                theme: theme,
                                onActivate: { activate(album) },
                                onPlay: { play(album) },
                                onEnqueue: { enqueue(album) }
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

    private var header: some View {
        HStack(spacing: 12) {
            Text("Albums")
                .font(theme.vars.bodyFont.weight(.semibold))
                .foregroundStyle(theme.playlistDurationText)
            if !albums.isEmpty {
                Text("\(albums.count)")
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

            HStack(spacing: 6) {
                Image(systemName: "photo")
                    .font(.system(size: 10))
                    .foregroundStyle(theme.playlistDurationText)
                Slider(value: $thumbPx, in: 96...256, step: 8)
                    .frame(width: 120)
                Image(systemName: "photo.fill")
                    .font(.system(size: 14))
                    .foregroundStyle(theme.playlistDurationText)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(theme.background)
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
}

// MARK: - Album cell

private struct AlbumCell: View {
    let album: AlbumGroup
    let thumbPx: Double
    let theme: SkinTheme
    let onActivate: () -> Void
    let onPlay: () -> Void
    let onEnqueue: () -> Void

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
                        Text("· \(year)")
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
        .help("\(displayArtist) — \(displayAlbum)")
        .onAppear(perform: loadArt)
    }

    private var displayAlbum: String {
        album.isNoAlbum ? "(no album)" : (album.album.isEmpty ? "Unknown Album" : album.album)
    }
    private var displayArtist: String {
        album.albumArtist.isEmpty ? "Unknown Artist" : album.albumArtist
    }

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
