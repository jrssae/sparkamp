# Sparkamp

A compact, fast, open-source Winamp-style music player for the GNOME desktop and MacOS — built in Rust with GTK4/Swift.

> **v1.3.2** — see [What's New](#whats-new-v132) for everything added in this release.

Like the project and want to support it? [Buy me a kofi](https://ko-fi.com/sparkamp) to donate to my AI tokens

---

There are a number of various Winamp clones and other audio players available for linux and MacOS — but the specific combination of features that made Winamp my favorite audio player does not exist in the way I want it to in any other audio player I've found. Sparkamp is a personal attempt to build exactly that: an audio player that gives me the things from Winamp that I miss most since leaving Windows. If those are the things you've been missing too, this might be for you.

> **This project is entirely vibe coded.** I am neither a programmer nor a designer — every line of code was written by Claude (Anthropic's AI assistant) and Big Pickle (when I ran out of tokens for the week). Human coders and designers are genuinely welcome and actively encouraged to contribute. If you see something that can be done better, please open a PR. I have no idea what I'm doing and some experience would be beneficial. The goal is a great piece of software, not a monument to any particular development process.

---

## What's New (v1.3.2)

An accessibility and desktop-integration release, with one data-loss fix in the terminal UI.

- **Pressing "/" in the terminal UI no longer wipes your playlist** — it cleared every track in the playlist view while meaning "search" one screen over in the Media Library. It searches in both places now, and clearing moved into the "o" ops popup as Remove All, beside the other whole-playlist actions.
- **The keys other GNOME apps use now work** — F1 for help, Ctrl+F to search, Ctrl+? for the shortcut list, and Ctrl+, for Settings, replacing the non-standard Ctrl+. Alt access keys open the playlist window's menus and the Settings tabs, and where several keys do the same thing the help window lists them one per line instead of running them together.
- **Text follows your desktop's text size** — sizes render in points rather than pixels, so GNOME's large-text accessibility setting actually enlarges Sparkamp. Existing skins are untouched; they still declare pixels and keep the sizes they set.
- **A screen reader can drive the player** — every icon-only button has a real name instead of announcing its glyph, the Settings sliders are labelled, and a track row reads as one sentence rather than a run of disconnected cells.
- **Failures that don't need a decision no longer stop you** — recoverable errors appear as a toast you can ignore instead of a dialog you must dismiss. Confirmations that really are destructive stay modal.
- **Empty views say what to do** — every Media Library view that can be empty shows a placeholder with an icon and a next step, including the disc overview, which was a single line of grey text.
- **The album gallery reflows as you resize** — it held five columns and clipped the covers however narrow the window got; it drops to four, three, two or one now. Its zoom-in button also drew an empty box rather than a "+".
- **Buttons stop falling off the edge of narrow windows** — the playlist editor's eleven-button row and the Files view's ten-button row were cut off whenever the window was smaller than they were, taking "Play" and "Remove" with them. Both wrap onto extra lines now, and every Media Library view shrinks to fit instead of holding its widest layout.

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
./target/release/sparkamp           # GTK4 graphical interface
./target/release/sparkamp --tui     # Terminal UI
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
./target/release/sparkamp --tui
```

---

## Display backend and renderer

Sparkamp runs on Wayland or X11, and X11/XLibre is a supported choice rather
than a fallback of last resort. **Settings → Appearance → Graphics** shows which
display backend and GSK renderer the running instance actually got, and lets you
pick either for the next launch.

The backend defaults to **Automatic**, which on a Wayland session starts a
throwaway helper process that opens a display and exits. If that helper dies on
a signal, Sparkamp uses X11 instead — some compositors crash GTK's Wayland
backend outright (COSMIC 1.7.0 with GTK 4.16 segfaults during display setup),
and a crash in a child is survivable where the same crash in the player is not.
The verdict is remembered per compositor and GTK version, so the check runs once
rather than on every launch, and re-runs by itself after a runtime upgrade.

Both settings can be overridden for a single run, which is the way back from a
choice that leaves you with no window to change it in:

```bash
sparkamp --backend=x11        # auto | wayland | x11
sparkamp --renderer=cairo     # auto | gl | vulkan | cairo
```

Neither flag writes to the config file; use the Settings dropdowns to make a
choice stick. `GDK_BACKEND` and `GSK_RENDERER` are honoured too, and are left
alone unless one of the flags above is given.

---

## Contributing

All contributions are welcome — bug fixes, new features, refactoring, documentation, design feedback. Since the codebase was AI-generated, there are almost certainly places where a human programmer would make different (better) choices. Don't be shy about pointing those out or just fixing them directly.

Please open an issue before starting large feature work so we can coordinate.

---

## License

[GNU Affero General Public License v3.0](https://www.gnu.org/licenses/agpl-3.0.html) (AGPL-3.0)
