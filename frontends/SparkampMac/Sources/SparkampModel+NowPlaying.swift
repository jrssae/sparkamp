import AppKit
import MediaPlayer

// MARK: - macOS Now Playing (Control Center / media keys / AirPods)
//
// Feeds `MPNowPlayingInfoCenter` (the OS media card) and wires
// `MPRemoteCommandCenter` (media keys, Control Center transport, AirPods taps,
// lock-screen scrub) back to the model's existing transport methods. The
// mirror of the Linux MPRIS integration; both read the SAME core now-playing
// data (`sparkamp_now_playing_*` / the `nowPlaying` snapshot).
//
// Hooks: `updateNowPlayingCenter()` is called from `refreshCurrentTrackInfo()`
// (track changes) and from `tick()` on a play/pause/stop transition. Remote
// commands are registered lazily on the first update (guarded by
// `nowPlayingConfigured`).

extension SparkampModel {

    /// Register the OS remote-command handlers once. Each routes to the model's
    /// existing transport method (which already funnels through the FFI +
    /// refreshes), so behavior matches the in-app buttons exactly.
    func configureRemoteCommands() {
        guard !nowPlayingConfigured else { return }
        nowPlayingConfigured = true

        let cc = MPRemoteCommandCenter.shared()

        cc.playCommand.isEnabled = true
        cc.playCommand.addTarget { [weak self] _ in
            self?.play(); return .success
        }
        cc.pauseCommand.isEnabled = true
        cc.pauseCommand.addTarget { [weak self] _ in
            self?.pause(); return .success
        }
        cc.stopCommand.isEnabled = true
        cc.stopCommand.addTarget { [weak self] _ in
            self?.stop(); return .success
        }
        cc.togglePlayPauseCommand.isEnabled = true
        cc.togglePlayPauseCommand.addTarget { [weak self] _ in
            self?.togglePlay(); return .success
        }
        cc.nextTrackCommand.isEnabled = true
        cc.nextTrackCommand.addTarget { [weak self] _ in
            self?.next(); return .success
        }
        cc.previousTrackCommand.isEnabled = true
        cc.previousTrackCommand.addTarget { [weak self] _ in
            self?.prev(); return .success
        }
        // Lock-screen / Control Center scrub.
        cc.changePlaybackPositionCommand.isEnabled = true
        cc.changePlaybackPositionCommand.addTarget { [weak self] event in
            guard let self,
                  let e = event as? MPChangePlaybackPositionCommandEvent,
                  self.duration > 0
            else { return .commandFailed }
            self.seek(to: e.positionTime / self.duration)
            // Reflect the new elapsed time immediately (the model's `position`
            // only refreshes on the next tick).
            var info = MPNowPlayingInfoCenter.default().nowPlayingInfo ?? [:]
            info[MPNowPlayingInfoPropertyElapsedPlaybackTime] = e.positionTime
            MPNowPlayingInfoCenter.default().nowPlayingInfo = info
            return .success
        }
    }

    /// Push the current track's metadata + playback state to the OS Now Playing
    /// card. Called on track / play-state changes (not per tick — macOS
    /// extrapolates elapsed time from the rate + timestamp).
    func updateNowPlayingCenter() {
        configureRemoteCommands()

        let center = MPNowPlayingInfoCenter.default()

        // Nothing loaded → clear the card.
        if currentTitle.isEmpty && currentArtist.isEmpty && nowPlaying == nil {
            center.nowPlayingInfo = nil
            center.playbackState = .stopped
            return
        }

        var info: [String: Any] = [:]
        info[MPMediaItemPropertyTitle] = currentTitle.isEmpty ? "Unknown" : currentTitle
        if !currentArtist.isEmpty {
            info[MPMediaItemPropertyArtist] = currentArtist
        }
        // Album lives in the curated now-playing tag rows.
        if let album = nowPlaying?.tags.first(where: { $0.0 == "Album" })?.1, !album.isEmpty {
            info[MPMediaItemPropertyAlbumTitle] = album
        }
        if duration > 0 {
            info[MPMediaItemPropertyPlaybackDuration] = duration
        }
        info[MPNowPlayingInfoPropertyElapsedPlaybackTime] = max(0, position)
        info[MPNowPlayingInfoPropertyPlaybackRate] = isPlaying ? 1.0 : 0.0

        if let path = nowPlaying?.artworkPath, !path.isEmpty,
           let image = NSImage(contentsOfFile: path) {
            let size = image.size
            info[MPMediaItemPropertyArtwork] = MPMediaItemArtwork(boundsSize: size) { _ in image }
        }

        center.nowPlayingInfo = info
        center.playbackState = isPlaying ? .playing : (isPaused ? .paused : .stopped)
    }
}
