import AppKit

/// Whether the lyrics window tracks a fixed song or the currently-playing one
/// (F15 revision, point 4).
enum LyricsMode: Hashable {
    case specific
    case current
}

/// The one track a surface would open lyrics for, resolved from its current
/// selection. Surfaces publish `nil` when nothing is selected or when more
/// than one row is — the `l` key is deliberately a single-row action.
struct LyricsTarget: Equatable {
    let path: String
    let artist: String
    let title: String
    let albumArtist: String
}

/// Decoded shape of `sparkamp_lyrics_view`'s JSON return.
private struct LyricsViewDTO: Decodable {
    let title: String
    let body: String
    let has_body: Bool
    let search_url: String
}

// View lyrics (F15). The whole view — fresh USLT read, marquee title, and
// search-URL encoding — is built by the Rust core behind `sparkamp_lyrics_view`
// and returned as one JSON blob, so Swift never re-derives it. The window
// ALWAYS opens now (no lyrics → "No lyrics available"); the DuckDuckGo search
// is an in-window button, not an alternate path.
extension SparkampModel {
    /// Open (or refresh) the lyrics window for one track. `bumpRequest` raises
    /// the singleton window; the Current-mode live refresh passes `false` so it
    /// only updates the shown content.
    func loadLyrics(path: String, artist: String, title: String, albumArtist: String,
                    mode: LyricsMode, bumpRequest: Bool) {
        let ptr = path.withCString { p in
            artist.withCString { a in
                title.withCString { t in
                    albumArtist.withCString { aa in
                        sparkamp_lyrics_view(p, a, t, aa)
                    }
                }
            }
        }
        guard let ptr else { return }
        let json = String(cString: ptr)
        sparkamp_free_string(ptr)
        guard let data = json.data(using: .utf8),
              let dto = try? JSONDecoder().decode(LyricsViewDTO.self, from: data)
        else { return }

        lyricsTitle = dto.title
        lyricsText = dto.has_body ? dto.body : ""   // "" → view shows "No lyrics available"
        lyricsSearchURL = dto.search_url
        lyricsEditPath = path
        lyricsMode = mode
        lyricsVisible = true
        if bumpRequest { lyricsRequest &+= 1 }
    }

    /// Open the lyrics window for `path`. `mode` seeds the This-song/Now-playing
    /// toggle: playlist/ML rows pass `.specific`; the now-playing affordance
    /// passes `.current`.
    func viewOrSearchLyrics(path: String, artist: String, title: String, albumArtist: String,
                            mode: LyricsMode = .specific) {
        // Toggle: opening lyrics for the track the window is already showing
        // closes it; a different track falls through and retargets the window.
        // `lyricsEditPath` is the shown track (Current mode keeps it in step
        // with playback), so this matches "same track → close" across modes.
        // Setting the flag false is what PlayerWindow's onChange dismisses on.
        if lyricsVisible && lyricsEditPath == path {
            lyricsVisible = false
            return
        }
        loadLyrics(path: path, artist: artist, title: title, albumArtist: albumArtist,
                   mode: mode, bumpRequest: true)
    }

    /// View lyrics for the playlist row at `index` (row menu, and the A1
    /// "Lyrics" affordance which passes the current track index in `.current`
    /// mode). Resolves path + tags from the loaded `playlistItems`.
    func viewOrSearchLyricsForPlaylist(index: Int, mode: LyricsMode = .specific) {
        guard index >= 0,
              let item = playlistItems.first(where: { $0.id == index }),
              let path = playlistTrackPath(index: index)
        else { return }
        viewOrSearchLyrics(path: path, artist: item.artist, title: item.title,
                           albumArtist: item.albumArtist, mode: mode)
    }

    /// The `l` key. Which track it opens depends on where the user is looking:
    /// a selected row in the Media Library or the active playlist, and
    /// otherwise the playing track — the same thing the A1 panel's "Lyrics"
    /// button does, in follow-the-track mode. A selection of zero or several
    /// rows does nothing, matching the row menus, which only offer
    /// "View/Search Lyrics" for exactly one row.
    func openLyricsForFocusedSurface() {
        // Window titles rather than scene ids: `NSApp.keyWindow` is an AppKit
        // window and SwiftUI does not expose its scene id here. Every Media
        // Library tab — including the disc and device views, which live inside
        // that window — reports the one title, which is exactly the grouping
        // `lyricsTargetML` wants.
        switch NSApp.keyWindow?.title {
        case "Media Library":
            open(target: lyricsTargetML)
        case "Playlist":
            open(target: lyricsTargetPlaylist)
        default:
            viewOrSearchLyricsForPlaylist(index: currentIndex, mode: .current)
        }
    }

    private func open(target: LyricsTarget?) {
        guard let t = target else { return }
        viewOrSearchLyrics(path: t.path, artist: t.artist, title: t.title,
                           albumArtist: t.albumArtist)
    }

    /// Retarget an open Current-mode lyrics window onto the now-playing track.
    /// Called from the lyrics window's now-playing observer; a no-op unless the
    /// window is open in Current mode.
    func refreshCurrentLyricsIfNeeded() {
        // Deliberately NOT gated on `lyricsVisible`. The only caller is the
        // live `LyricsView`, so the window is on screen by construction, and
        // the flag can lag reality: macOS restores the "lyrics-viewer" Window
        // scene at launch, which puts the view back on screen with the model
        // still at its `false` default. That made every Now-playing refresh
        // bail on a window the user was looking at.
        guard lyricsMode == .current, currentIndex >= 0,
              let item = playlistItems.first(where: { $0.id == currentIndex }),
              let path = playlistTrackPath(index: currentIndex)
        else { return }
        loadLyrics(path: path, artist: item.artist, title: item.title,
                   albumArtist: item.albumArtist, mode: .current, bumpRequest: false)
    }
}
