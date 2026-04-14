import SwiftUI
import AppKit

// MARK: - Artwork zoom window

/// Displays the current track's album art at up to 512×512, scaled to fit the window.
/// The image updates automatically when a different track is loaded in the ID3 editor.
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
                Text("No artwork")
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .background(Color.black)
        .onDisappear {
            model.artworkWindowVisible = false
        }
    }
}
