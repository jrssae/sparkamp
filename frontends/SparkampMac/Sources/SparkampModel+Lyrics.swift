import AppKit

/// Whether the lyrics window tracks a fixed song or the currently-playing one
/// (F15 revision, point 4).
enum LyricsMode: Hashable {
    case specific
    case current
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

    /// Retarget an open Current-mode lyrics window onto the now-playing track.
    /// Called from the lyrics window's now-playing observer; a no-op unless the
    /// window is open in Current mode.
    func refreshCurrentLyricsIfNeeded() {
        guard lyricsVisible, lyricsMode == .current, currentIndex >= 0,
              let item = playlistItems.first(where: { $0.id == currentIndex }),
              let path = playlistTrackPath(index: currentIndex)
        else { return }
        loadLyrics(path: path, artist: item.artist, title: item.title,
                   albumArtist: item.albumArtist, mode: .current, bumpRequest: false)
    }
}
