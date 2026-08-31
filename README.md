# Sparkamp

A compact, fast, open-source Winamp-style music player for the GNOME desktop and MacOS — built in Rust with GTK4/Swift.

> **v1.3.3** — see [What's New](#whats-new-v133) for everything added in this release.

Like the project and want to support it? [Buy me a kofi](https://ko-fi.com/sparkamp) to donate to my AI tokens

---

There are a number of various Winamp clones and other audio players available for linux and MacOS — but the specific combination of features that made Winamp my favorite audio player does not exist in the way I want it to in any other audio player I've found. Sparkamp is a personal attempt to build exactly that: an audio player that gives me the things from Winamp that I miss most since leaving Windows. If those are the things you've been missing too, this might be for you.

> **This project is entirely vibe coded.** I am neither a programmer nor a designer — every line of code was written by Claude (Anthropic's AI assistant) and Big Pickle (when I ran out of tokens for the week). Human coders and designers are genuinely welcome and actively encouraged to contribute. If you see something that can be done better, please open a PR. I have no idea what I'm doing and some experience would be beneficial. The goal is a great piece of software, not a monument to any particular development process.

---

## What's New (v1.3.3)

A Flatpak release. Everything here was found by running the real sandboxed build against real hardware — none of it showed up in the test suite, because the tests run outside the sandbox where the permissions and tools already exist. Disc support, read-write access to removable media, online lookups and a fallback for compositors that crash GTK.

- **Sparkamp starts on compositors that crash GTK's Wayland support** — it opens a throwaway helper first, and if that dies it switches to X11 and says so, instead of vanishing before a window ever appears. Settings → Appearance → Graphics shows which display backend and renderer you actually got and lets you pick either; --backend=x11 and --renderer=cairo override them for one run, so a choice that leaves you with no window is never a dead end.
- **Audio CDs work in the Flatpak at all** — the sandbox had no access to disc drives, so a CD read as a data disc with no tracks and Eject failed with “device not found”. Playing, reading and ripping all work now.
- **Ripping a CD produces files** — the CD reader ripping depends on has never been part of the runtime, so every rip failed on a missing component. It ships with Sparkamp now and reads with error correction, which matters on a scratched disc.
- **Disc and track names are read from the disc** — CD-TEXT needed a tool the sandbox didn’t have, so a disc never identified itself even when it carried its own title.
- **Looking a disc up online works** — Sparkamp had no network permission at all, so gnudb lookups failed as though the service were down.
- **Burning and erasing work** — both needed tools the sandbox didn’t have. Those buttons failed before; they now do what they say.
- **USB sticks and SD cards can be read and written** — the app could see a device and list nothing from it, because the sandbox had no access to where removable media is mounted.
- **A device that can’t be read says why** — an unreadable stick or disc showed as empty, which is indistinguishable from one with no music on it. It now explains what to do, and hides the file list and the actions that can’t work rather than offering them.
- **The disc view matches the device view** — same header band with the disc’s own buttons in it, and a disc’s ordinary state (“Blank disc — ready to burn”) is no longer painted in the same alarm colour as a real fault.
- **Right-click menus use your skin’s font** — every context menu in the app rendered in the wrong typeface, whatever the skin said.
- **Tracks on a CD are no longer marked broken** — a disc track isn’t a file on disk, and the check for missing files counted it as gone while it was playing.
- **The app icon appears in more desktops’ launchers** — it shipped at one large size, which COSMIC’s launcher doesn’t scale down, so it came up blank there.
- **Built on GTK 4.22** — the Flatpak moves from the GNOME 47 runtime to GNOME 50, which went out of support in October 2025.

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
