# Sparkamp

A compact, fast, open-source Winamp-style music player for the GNOME desktop and MacOS — built in Rust with GTK4/Swift.

> **v1.4.1**, see [What's New](#whats-new-v141) for everything added in this release.

Like the project and want to support it? [Buy me a kofi](https://ko-fi.com/sparkamp) to donate to my AI tokens

---

There are a number of various Winamp clones and other audio players available for linux and MacOS — but the specific combination of features that made Winamp my favorite audio player does not exist in the way I want it to in any other audio player I've found. Sparkamp is a personal attempt to build exactly that: an audio player that gives me the things from Winamp that I miss most since leaving Windows. If those are the things you've been missing too, this might be for you.

> **This project is entirely vibe coded.** I am neither a programmer nor a designer — every line of code was written by Claude (Anthropic's AI assistant) and Big Pickle (when I ran out of tokens for the week). Human coders and designers are genuinely welcome and actively encouraged to contribute. If you see something that can be done better, please open a PR. I have no idea what I'm doing and some experience would be beneficial. The goal is a great piece of software, not a monument to any particular development process.

---

## What's New (v1.4.1)

A patch release with minor fixes for MacOS.

- **Found and fixed a lag issue with the tag editor.** Elapsed time was published ten times a second on an object every window observed, so a window showing no clock still rebuilt on every tick. The editor was the worst case, rebuilding two dozen rows and the lyrics box ten times a second for two values it never displayed, which is why the lag grew with the size of the lyrics.
- **The genre field typeahead matches any part of the word.** Typing "fus" now offers "Fast Fusion" as well as "Fusion". Previously, it only matched from the start of the typed word.
- **The bars and waveform visualizers' animation freeze is fixed.** They had been repainting only as a side effect of the clock above, so decoupling it stopped them dead. They now run at 60 Hz on a schedule of their own, and draw each color zone in one pass instead of one call per bar or per sample.
- **Fixed an issue where editing the ID3 year value wasn't saved/displayed correctly.** ID3's older year frame was offered as a second, independent "Year (legacy)" field, and saving replayed it over the year just set. Files whose tags carry a stray byte-order mark also read their year, track and disc numbers as empty.
- **The Media Library remembers column widths and order.** Both were saved correctly and never read back, so leaving the Files view or quitting reset the layout every time.

**See releases for historic release notes**

---

## Tech Stack

| Layer | Technology |
|---|---|
| Language | Rust (2024 edition) |
| GNOME frontend | GTK4 (`gtk4 = "0.9"`) |
| CLI | Clap |
| macOS frontend | Swift / SwiftUI + Rust FFI staticlib |
| TUI | Ratatui + Crossterm |
| Audio backend (Linux, TUI) | GStreamer (`gstreamer = "0.22"`) |
| Audio backend (macOS) | AVFoundation (`AVAudioEngine`) |
| Equalizer | GStreamer `equalizer-10bands` on Linux, `AVAudioUnitEQ` on macOS |
| Optical discs | libcdio / cdparanoia on Linux, DiscRecording on macOS |
| Duration probing | Symphonia, plus GStreamer Discoverer on Linux and AVFoundation on macOS |
| Parallel probing | Rayon |
| Metadata | id3 + Symphonia (OGG/FLAC/Opus fallback) |
| Config / playlist | TOML + Serde |
| Media library | SQLite via `rusqlite` (bundled, no system dep) |


---

## Building

On Linux you need Rust (stable, 2024 edition) and the GStreamer development
libraries. macOS needs neither. See below.

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

Requires Xcode Command Line Tools and Rust. Nothing from Homebrew: audio goes
through AVFoundation and discs through DiscRecording, both part of macOS.
```bash
xcode-select --install
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
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

See [CONTRIBUTING.md](CONTRIBUTING.md) for the build and test commands, and
[CLA.md](CLA.md), which a first pull request should say it agrees to.

---

## License

[GNU Affero General Public License v3.0](https://www.gnu.org/licenses/agpl-3.0.html) (AGPL-3.0)

All of it: the core, the GTK frontend, the TUI, the macOS app and the FFI
bridge between them. There is no separately-licensed part.

### On distribution through app stores

Sparkamp is built for the Mac App Store as well as for direct download, and
the App Store's terms conflict with the AGPL — they impose restrictions on
recipients that a copyleft licence forbids adding. That conflict is real, and
it is worth explaining how this project sits on the right side of it, because
seeing an AGPL app in the App Store reasonably raises the question.

**A licensor is not bound by the licence they grant.** The AGPL here is a grant
made to everyone else. It does not constrain the copyright holder, who cannot
infringe their own copyright. Every commit in this repository is by one person,
so the App Store build is that person distributing their own work — not a
licensee redistributing under terms the AGPL forbids.

This is the arrangement Signal uses for its AGPL apps, and it is why
[the CLA](CLA.md) exists: it keeps the rights consolidated so the arrangement
keeps working. Section 8 of that agreement is the promise back — contributions
stay under an OSI-approved open-source licence, whatever else is done with
them.

The source published here is the source that is built for every channel. There
is no proprietary variant.
