import AppKit

// View/Search Lyrics (F15). The whole decision — fresh USLT read, Show-vs-Search
// branch, and search-URL encoding — lives in the Rust core behind
// `sparkamp_lyrics_action`, so Swift never re-derives it; this file only routes
// the result to the viewer window or the default browser.
extension SparkampModel {
    /// Resolve lyrics for one track: saved USLT opens (or raises) the read-only
    /// viewer window; no lyrics opens the default browser on a DuckDuckGo search.
    func viewOrSearchLyrics(path: String, artist: String, title: String, albumArtist: String) {
        var kind: UInt32 = 1
        let ptr = path.withCString { p in
            artist.withCString { a in
                title.withCString { t in
                    albumArtist.withCString { aa in
                        sparkamp_lyrics_action(p, a, t, aa, &kind)
                    }
                }
            }
        }
        guard let ptr else { return }
        let body = String(cString: ptr)
        sparkamp_free_string(ptr)

        if kind == 0 {
            lyricsTitle = title.trimmingCharacters(in: .whitespaces).isEmpty
                ? URL(fileURLWithPath: path).deletingPathExtension().lastPathComponent
                : title
            lyricsText = body
            lyricsEditPath = path
            lyricsVisible = true
            lyricsRequest &+= 1
        } else if let url = URL(string: body) {
            NSWorkspace.shared.open(url)
        }
    }

    /// View/Search lyrics for the active-playlist row at `index` (used by the
    /// playlist row menu and the A1 "Lyrics" link, which passes the current
    /// track index). Resolves path + tags from the loaded `playlistItems`.
    func viewOrSearchLyricsForPlaylist(index: Int) {
        guard index >= 0,
              let item = playlistItems.first(where: { $0.id == index }),
              let path = playlistTrackPath(index: index)
        else { return }
        viewOrSearchLyrics(path: path, artist: item.artist, title: item.title,
                           albumArtist: item.albumArtist)
    }
}
