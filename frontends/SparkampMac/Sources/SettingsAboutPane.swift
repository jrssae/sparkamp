import SwiftUI
import AppKit

// Split out of SettingsWindow.swift on 2026-08-12: the file was 1,087
// lines of five unrelated panes. They were already siblings of
// `SettingsView` rather than nested in it, so each moved whole — the
// only edit is `private struct` -> `struct`, because file-private would
// now hide the pane from the host that instantiates it.

// MARK: - About pane

struct AboutPane: View {
    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack(spacing: 16) {
                Image(nsImage: NSApp.applicationIconImage)
                    .resizable()
                    .frame(width: 64, height: 64)

                VStack(alignment: .leading, spacing: 4) {
                    Text("Sparkamp")
                        .font(.title2.bold())
                    Text("Version \(Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "")")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                    Text("A compact, fast, open-source Winamp-style music player with the backend built in Rust and support for UI in GNOME desktop with GTK4 & macOS with Swift.")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }

            Divider()

            VStack(alignment: .leading, spacing: 6) {
                Text("Engine")
                    .font(.headline)
                Text("AVFoundation: AVAudioEngine, AVAudioUnitEQ, AVAudioMixerNode")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            }

            VStack(alignment: .leading, spacing: 6) {
                Text("License")
                    .font(.headline)
                Button("GNU Affero General Public License v3 (AGPL-3.0)") {
                    NSWorkspace.shared.open(
                        URL(string: "https://www.gnu.org/licenses/agpl-3.0.html")!
                    )
                }
                .buttonStyle(.link)
                .font(.subheadline)
                // Sections 15 and 16 of that licence already say this, and
                // nobody reads them. Saying it here in plain words costs a
                // line and is the honest thing to put in front of someone
                // before they point the app at their music.
                Text("Sparkamp is made in good faith and comes with no warranty. If it loses data or breaks something, that risk is yours. Sections 15 and 16 of the licence say this in legal terms.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            VStack(alignment: .leading, spacing: 6) {
                Text("Get the latest")
                    .font(.headline)
                Text("Source code, releases, and issue tracking are hosted on GitHub. Clone the repository or grab the latest build there.")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                Button("github.com/jrssae/sparkamp") {
                    NSWorkspace.shared.open(
                        URL(string: "https://github.com/jrssae/sparkamp")!
                    )
                }
                .buttonStyle(.link)
                .font(.subheadline)
            }

            Spacer()

            // Nominative use of another product's name is fine, but saying so
            // explicitly is what actually lowers the risk. A trademark symbol
            // would not: those are used by the owner of a mark, so printing
            // one here would read as Sparkamp claiming it.
            Text("Winamp is a trademark of its respective owner. Sparkamp is an independent project, not affiliated with or endorsed by them.")
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            // Below the Spacer, so it sits at the bottom of the pane. The App
            // Store requires a privacy policy URL in the listing; having it
            // here too means it is reachable from inside the app rather than
            // only from the store page.
            Button("Privacy Policy") {
                NSWorkspace.shared.open(
                    URL(string: "https://github.com/jrssae/sparkamp/blob/main/PRIVACY.md")!
                )
            }
            .buttonStyle(.link)
            .font(.subheadline)
        }
        .padding(24)
    }
}
