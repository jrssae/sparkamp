import SwiftUI
import AppKit

// MARK: - Artwork window (A6 — singleton, follows the current track)

/// Displays album art at up to 512×512, scaled to fit the window. A single
/// (`Window`, not `WindowGroup`) instance — SwiftUI reuses/refronts it rather
/// than creating a second one — serves two related but distinct uses:
///
/// 1. **Follow-the-track mode** (`k` key, or clicking the A1 panel's art):
///    `model.openArtworkWindow()` sets `artworkFollowsPlayback = true`, and
///    every `refreshNowPlaying()` (i.e. every track change) re-pushes
///    `nowPlaying.artworkPath` into `model.artworkImage` via
///    `loadFollowedArtwork()` — this window just displays whatever is there.
/// 2. **Static zoom** (ID3 editor's artwork thumbnail tap, Media Library's
///    "View Art"): those call sites set `artworkFollowsPlayback = false` and
///    push one fixed image, which this window shows without it being
///    overwritten by the next track change.
///
/// Shows the same "No artwork available" placeholder as the A1 panel when
/// there's nothing to show.
struct ArtworkView: View {
    @EnvironmentObject var model: SparkampModel

    var body: some View {
        Group {
            if let img = model.artworkImage {
                Image(nsImage: img)
                    .resizable()
                    .scaledToFit()
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                VStack(spacing: 10) {
                    Image(nsImage: NSApp.applicationIconImage)
                        .resizable()
                        .frame(width: 64, height: 64)
                        .opacity(0.5)
                    Text("No artwork available")
                        .foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .background(Color.black)
        .onAppear {
            if model.artworkFollowsPlayback { model.loadFollowedArtwork() }
        }
        .onDisappear {
            model.artworkWindowVisible = false
            // Closing always exits follow mode — the next open should default
            // to whichever call site (k, or a static zoom) opens it next,
            // not silently inherit a stale mode from this session.
            model.artworkFollowsPlayback = false
        }
    }
}

// MARK: - A6 open-or-focus + follow-playback

extension SparkampModel {
    /// Open (or, if already open, focus) the standalone album-art window in
    /// "follow the current track" mode. Wired to the `k` key and the A1
    /// panel's art tap. Mirrors the GTK A6 singleton's `open_or_focus`:
    /// bumping `artworkWindowRequest` makes `WindowManagerModifier` call
    /// `openWindow` unconditionally (same idiom as `id3Request`), so a
    /// repeat press re-fronts the existing window instead of no-op'ing.
    func openArtworkWindow() {
        artworkFollowsPlayback = true
        loadFollowedArtwork()
        artworkWindowVisible = true
        artworkWindowRequest &+= 1
    }

    /// Re-read the artwork for the current `nowPlaying` snapshot and push it
    /// into `artworkImage`. No-op unless follow mode is on (a static zoom
    /// from the ID3 editor / Media Library must not be clobbered by the next
    /// track change). Called from `openArtworkWindow()` and from
    /// `refreshNowPlaying()` on every track change.
    func loadFollowedArtwork() {
        guard artworkFollowsPlayback else { return }
        let path = nowPlaying?.artworkPath ?? ""
        artworkImage = path.isEmpty ? nil : NSImage(contentsOfFile: path)
    }
}
