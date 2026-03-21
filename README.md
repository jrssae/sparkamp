# SparkAmp

A compact, fast, open-source Winamp-style music player for the GNOME desktop — built in Rust with GTK4.

The Linux audio player ecosystem is genuinely strong — but the specific combination of features that made Winamp's workflow so satisfying has been hard for me to find in one single app. SparkAmp is a personal attempt to build exactly that: an audio player that gives me the things from Winamp that I miss most since leaving Windows. If those are the things you've been missing too, this might be for you.

> **This project is entirely vibe coded.** I am neither a programmer nor a designer — every line of code was written by Claude (Anthropic's AI assistant). Human coders and designers are genuinely welcome and actively encouraged to contribute. If you see something that can be done better, please open a PR. I have no idea what I'm doing and some experience is very beneficial. The goal is a great piece of software, not a monument to any particular development process.

---

## Current Features

- **GTK4 GUI** with automatic light/dark theme support and system accent color
- **Winamp-style transport controls** — z / x / c / v / b keyboard bindings
- **Scrolling marquee** showing current track title and artist
- **Seek bar** with scrub-before-play support
- **Volume slider**
- **Playlist window** — drag-to-reorder, per-track duration display, broken/missing file indicators
- **Jump-to-track** — press `j` to open a live search window, navigate with arrow keys, press Enter to jump and play
- **Repeat & shuffle** — cycle repeat (off / song / all) with `r`; toggle shuffle with `s`; clearly labelled `🔁1` / `🔁A`
- **10-band equalizer** — built-in GStreamer EQ with 7 presets (Flat, Rock, Pop, Jazz, Classical, Bass Boost, Treble Boost); accessible via the EQ button or `u` key
- **ID3 tag viewer/editor** — view and edit title, artist, album, year, track, genre, comment; access custom frames; press `d` in the TUI
- **Settings panel** — appearance (theme, custom skin), behaviour, visualizer mode, plugin directories
- **Background duration probing** — file lengths appear immediately on load without blocking the UI, with a persistent cache
- **Missing file detection** — files that disappear from disk are marked with a warning indicator
- **Duration cache** — probed durations persist to disk and appear instantly on next launch
- **Mini visualizer** — bars or oscilloscope mode, with a visualizer plugin API for custom animations
- **CLI / TUI mode** — a full terminal UI for headless or keyboard-only use (`sparkamp --tui`)
- **Playlist persistence** — saves and restores the playlist and player position between sessions
- **Skin system** — light and dark CSS skins, switchable at runtime; user skins at `~/.config/sparkamp/skins/`
- **Format support** — MP3, FLAC, OGG Vorbis, Opus, WAV, M4A; filetype plugin API for additional formats

---

## Roadmap

These are the directions the project is heading, in no particular order. Nothing here is committed to a timeline.

- **Media library** — artist/album browsing, full metadata display
- **Skin format and migration tool** — a new skin format with a migration path from classic Winamp `.wsz` skins
- **macOS support** — would be nice to have on a laptop
- **Equalizer UI polish** — save named custom presets, per-band labels in the GTK window

---

## Tech Stack

| Layer | Technology |
|---|---|
| Language | Rust (2024 edition) |
| GUI toolkit | GTK4 (`gtk4 = "0.9"`) |
| Audio backend | GStreamer (`gstreamer = "0.22"`) |
| Equalizer | GStreamer `equalizer-10bands` (gst-plugins-good) |
| Duration probing | Symphonia + GStreamer Discoverer |
| Parallel probing | Rayon |
| TUI | Ratatui + Crossterm |
| Metadata | id3 + Symphonia (OGG/FLAC/Opus fallback) |
| Config / playlist | TOML + Serde |
| CLI | Clap |

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

Then build and run:
```bash
cargo build --release
./target/release/sparkamp
```

For TUI mode:
```bash
./target/release/sparkamp --tui
```

---

## Keyboard Shortcuts

### Player window

| Key | Action |
|---|---|
| `z` | Previous track |
| `x` | Play |
| `c` | Pause / Resume |
| `v` | Stop |
| `b` | Next track |
| `j` | Jump to track (search) |
| `i` | Info / keyboard shortcuts |
| `p` | Toggle playlist window |
| `o` | Add file |
| `r` | Cycle repeat mode (off / 🔁1 / 🔁A) |
| `s` | Toggle shuffle |
| `u` | Equalizer |
| `d` | ID3 tag editor (TUI) |
| `e` | Settings (TUI) |
| `a` | Cycle visualizer mode |
| `←` / `→` | Seek backward / forward 5 s |
| `[` / `]` | Volume down / up |

### Playlist window

| Key | Action |
|---|---|
| `j` | Jump to track (search) |
| `Del` | Remove selected track |

### Jump window

| Key | Action |
|---|---|
| Type | Filter results live |
| `↑` / `↓` | Navigate results |
| `Enter` | Play selected track |
| `Esc` | Close |

### Equalizer (TUI)

| Key | Action |
|---|---|
| `←` / `→` | Select band |
| `↑` / `↓` | ±1 dB |
| `PgUp` / `PgDn` | ±3 dB |
| `p` | Cycle preset |
| `r` | Reset to flat |
| `t` | Toggle EQ on/off |
| `u` / `Esc` | Close |

---

## Contributing

All contributions are welcome — bug fixes, new features, refactoring, documentation, design feedback. Since the codebase was AI-generated, there are almost certainly places where a human programmer would make different (better) choices. Don't be shy about pointing those out or just fixing them directly.

Please open an issue before starting large feature work so we can coordinate.

---

## License

TBD — will be an OSI-approved open source license before the first public release.
