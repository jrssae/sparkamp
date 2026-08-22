# Sparkamp

A compact, fast, open-source Winamp-style music player for the GNOME desktop and MacOS — built in Rust with GTK4/Swift.

> **v1.3.1** — see [What's New](#whats-new-v131) for everything added in this release.

Like the project and want to support it? [Buy me a kofi](https://ko-fi.com/sparkamp) to donate to my AI tokens

---

There are a number of various Winamp clones and other audio players available for linux and MacOS — but the specific combination of features that made Winamp my favorite audio player does not exist in the way I want it to in any other audio player I've found. Sparkamp is a personal attempt to build exactly that: an audio player that gives me the things from Winamp that I miss most since leaving Windows. If those are the things you've been missing too, this might be for you.

> **This project is entirely vibe coded.** I am neither a programmer nor a designer — every line of code was written by Claude (Anthropic's AI assistant) and Big Pickle (when I ran out of tokens for the week). Human coders and designers are genuinely welcome and actively encouraged to contribute. If you see something that can be done better, please open a PR. I have no idea what I'm doing and some experience would be beneficial. The goal is a great piece of software, not a monument to any particular development process.

---

## What's New (v1.3.1)

A bug-fix release, focused on performance and drag-and-drop.

- **Drag anything to the playlist** — five Media Library views had no drag source at all — the album gallery, both disc views and both device views. All nine now drag, and dragging a container adds everything in it: an album its tracks, a saved playlist its tracks, a disc its tracks, a device its files.
- **Adding files does what the setting says** — "Default add file action" (Append or Replace) was ignored by drag-and-drop entirely, and applied inconsistently elsewhere — six places each decided for themselves. One rule now governs every route in: drag-and-drop, the Media Library, the command line, and files opened from the desktop. The setting was also mislabelled "Media library → playlist" in all three frontends, which described where it was first used rather than what it does.
- **The album gallery opens about twice as fast** — clicking Albums rebuilt the whole grid twice per click; the fold that reads the library moved into SQL, the grid fills in one operation instead of ~5,000, and returning to the gallery reuses what it already had.
- **Search the album view** in GTK and the TUI, matching macOS — by album title or artist, including the "(No album)" bucket, which now reads the same in all three frontends.
- **Drag-and-drop defects found on real hardware** — dragging a CD track added one entry named after the device node with no tags; a device row could not be dragged at all; an album could only be added once; and an external drop was silently discarded whenever playlist rows happened to be selected.

**See releases for historic release notes**

---

## Tech Stack

| Layer | Technology |
|---|---|
| Language | Rust (2024 edition) |
| GNOME frontend | GUI toolkit | GTK4 (`gtk4 = "0.9"`) |
| CLI | Clap |
| macOS frontend | Swift / SwiftUI + Rust FFI staticlib |
| TUI | Ratatui + Crossterm |
| Audio backend | GStreamer (`gstreamer = "0.22"`) |
| Equalizer | GStreamer `equalizer-10bands` (gst-plugins-good) |
| Duration probing | Symphonia + GStreamer Discoverer |
| Parallel probing | Rayon |
| Metadata | id3 + Symphonia (OGG/FLAC/Opus fallback) |
| Config / playlist | TOML + Serde |
| Media library | SQLite via `rusqlite` (bundled, no system dep) |


---

## Building

You need Rust (stable, 2024 edition) and the GStreamer development libraries.

**Fedora / Bazzite:**
```bash
sudo dnf install gstreamer1-devel gstreamer1-plugins-base-devel \
                 gstreamer1-plugins-good gstreamer1-plugins-bad-free \
                 gtk4-devel
```

**Ubuntu / Debian:**
```bash
sudo apt install libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
                 libgstreamer-plugins-bad1.0-dev \
                 libgtk-4-dev
```

Build the main binary:
```bash
cargo build --release
./target/release/sparkamp --ui      # GTK4 graphical interface
./target/release/sparkamp           # Terminal UI
```

**macOS — Standalone DMG (recommended, no dependencies):**

Download `Sparkamp-<version>.dmg` from the [Releases](../../releases) page, open it, and drag **Sparkamp** into Applications.

First launch: right-click the app → **Open** to bypass Gatekeeper (the app is ad-hoc signed, not notarized).  
Or from Terminal: `xattr -cr /Applications/SparkampMac.app`

**macOS — Build from source:**

Requires Xcode Command Line Tools, Rust, and GStreamer via Homebrew:
```bash
xcode-select --install
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
brew install gstreamer gst-plugins-base gst-plugins-good \
             gst-plugins-bad gst-plugins-ugly gst-libav mpg123
```

To build a self-contained DMG:
```bash
bash packaging/macos/build-dmg.sh
# → dist/Sparkamp-<version>.dmg
```

To build and run directly in Xcode, open `frontends/SparkampMac/SparkampMac.xcodeproj`. The Cargo build phase runs automatically and links the Rust static library.

> The Granite plasma visualizer is built in (`src/granite/`) — no separate plugin to build or install. Both the GTK and macOS frontends render it directly.

For TUI mode:
```bash
./target/release/sparkamp
```

---

## Contributing

All contributions are welcome — bug fixes, new features, refactoring, documentation, design feedback. Since the codebase was AI-generated, there are almost certainly places where a human programmer would make different (better) choices. Don't be shy about pointing those out or just fixing them directly.

Please open an issue before starting large feature work so we can coordinate.

---

## License

[GNU Affero General Public License v3.0](https://www.gnu.org/licenses/agpl-3.0.html) (AGPL-3.0)
