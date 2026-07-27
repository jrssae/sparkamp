import Foundation

/// Shared status-line formatter for the active playlist and the four Media
/// Library list views (Files, Playlist editor, Device detail, Disc-files
/// browser). Mirrors core `playlist_status_line` (src/playlist_status.rs)
/// EXACTLY — keep in sync. Produces `N tracks · MM:SS total · MM:SS
/// selected`; the selected clause is present only when `selectedSecs` is
/// non-nil. Durations roll over to `H:MM:SS` at/above one hour.
///
/// Originally a `static` on `PlaylistView` (phase 7); lifted here so every
/// caller shares one formatter instead of each view carrying its own copy.
func playlistStatusLine(count: Int, totalSecs: Int, selectedSecs: Int?) -> String {
    func hms(_ s: Int) -> String {
        let h = s / 3600, m = (s % 3600) / 60, sec = s % 60
        return h > 0 ? String(format: "%d:%02d:%02d", h, m, sec)
                     : String(format: "%d:%02d", m, sec)
    }
    let noun = count == 1 ? "track" : "tracks"
    var line = "\(count) \(noun) · \(hms(totalSecs)) total"
    if let sel = selectedSecs { line += " · \(hms(sel)) selected" }
    return line
}
