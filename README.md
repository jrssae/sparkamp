# Sparkamp

A compact, fast, open-source Winamp-style music player for the GNOME desktop — built in Rust with GTK4.

> **v0.3.0** — see [What's New](#whats-new-v030) for everything added in this release.

---

There are a number of various Winamp clones and other audio players available for linux — but the specific combination of features that made Winamp my favorite audio player does not exist in the way I want it to in any other audio player I've found. Sparkamp is a personal attempt to build exactly that: an audio player that gives me the things from Winamp that I miss most since leaving Windows. If those are the things you've been missing too, this might be for you.

> **This project is entirely vibe coded.** I am neither a programmer nor a designer — every line of code was written by Claude (Anthropic's AI assistant) and Big Pickle (when I ran out of tokens for the week). Human coders and designers are genuinely welcome and actively encouraged to contribute. If you see something that can be done better, please open a PR. I have no idea what I'm doing and some experience would be beneficial. The goal is a great piece of software, not a monument to any particular development process.

---

## What's New (v0.3.0)

### Waveform Visualizer
- **Real-audio waveform** — center-line (bipolar) oscilloscope display driven by actual PCM audio sampled from the GStreamer pipeline via a pad probe; smooth Winamp-style rendering with a 5-tap moving-average filter
- **1–6 configurable color zones** — zone 1 at the bottom (default dark green), zone 6 at the top (default red); same defaults as the bar visualizer; each zone color is independently configurable in Settings → Visualizer
- **Lines and Filled styles** — Lines draws each waveform segment colored by the zone it passes through; Filled fills the area between the waveform and the center baseline column-by-column per zone
- **Fullscreen mode** — press `f` or double-click the mini visualizer while in Waveform mode; the waveform expands to cover the entire display (OS-level fullscreen, no other windows visible); status toasts show playback state changes; `z x c v b r s` pass through to the player, `j` opens Jump-to-track overlaid on the fullscreen canvas, `i` shows keyboard shortcuts, `Esc` exits
- **Settings persisted** — zone count, zone colors, and style (Lines/Filled) are saved to TOML config and restored between sessions
- Inspired by the [LSaO Visualizer](https://github.com/aaronfbianchi/LSaO-visualizer)

### Duplicate Music Finder
- **Deduplicate Music** button in Settings → Media Library launches a dedicated window
- Background scan groups tracks by normalised artist+title metadata, with filename cross-matching as a fallback
- Groups are shown in a virtualised `TreeStore`+`TreeView` — smooth scrolling even with thousands of duplicate groups
- Confidence levels: **Probable** (metadata match, duration spread ≤ 10 s) and **Less likely** (duration spread > 10 s, or filename-only match)
- Right-click a group to **Add to playlist** or **Replace playlist**; right-click a track entry to **Open file location** or mark as **Not a duplicate** (removes it from the group without deleting the file)
- Cancel with confirmation prompt; close-during-scan also prompts
- Correct right-click row detection: widget→bin-window coordinate conversion ensures the column header never causes an off-by-one

### ID3 Editor improvements
- **Marquee updates immediately** after saving — works from any editor entry point (marquee click, `d` key, playlist right-click, Media Library)
- **Media Library DB record updated on save** — `rescan_track` re-reads the file's tags and upserts the row; ML window refreshes automatically if open
- **Artwork cache refreshed** on save

### Bug Fixes
- **Volume on startup** — saved volume was silently reset to 100% on every launch because `apply_eq_bands` (called during init) overwrote the GStreamer volume element via pre-amp. Fixed by tracking `user_volume` and `user_preamp` separately and always writing their product
- **ML search filter preserved across rebuilds** — background events (folder add, rescan, ID3 save) no longer reset the track list to the full library when a search query is active
- **EQ window close handler**, **periodic config save**, and **keyboard `s` shuffle mirror** — three persistent settings bugs fixed

### UI Polish
- **Clear (✕) button** added to the Media Library search bar
- **Clear (✕) button** added to the Jump-to-track search bar

---

## What's New (v0.2.0)

This release added the media library and resolved a number of accumulated issues.

### Media Library
- **SQLite-backed library** with watch folders and background rescanning
- **Files view** — sortable columns (track #, title, artist, album, duration, filename, year, genre, bitrate), live search with debounce
- **Playlists view** — saved M3U playlists, preview, set as current playlist
- **Add to Playlist** — select tracks in ML, click a playlist, tracks are appended
- **Add Folder** — pick any directory, audio files are scanned and added (quick-scan, background thread)
- **Remove tracks** — bulk remove with instant UI update (no DB re-query)
- **Rescan** — rescan all folders or individual folders
- **Customize columns** — show/hide columns, persisted per user
- **Watch folders** — manage watched directories in Settings → Media Library tab
- **Duplicate detection** — adding a folder that already exists triggers a rescan instead of a duplicate entry
- **DB indexes** — added on `tracks(artist)`, `tracks(title)`, `tracks(album)`, `tracks(folder_id)` for performance

### Bug Fixes
- **Fixed ML window lag** — sort model was set before initial load, causing O(n²) re-sorting on every insert. Now uses `splice()` for batch inserts and sets the sorter after load completes.
- **Fixed app not exiting when ML window open** — main window close handler now destroys the ML window
- **Settings window made non-modal** — was locked on top of the player, now independently movable
- **Pre-amp slider restored in EQ** — was accidentally removed, restored with full callback wiring
- **Fixed +Folder button in playlist window** — was using `open_multiple` (files only), now uses `FileDialog::select_folder()`
- **ML window state persistence** — `ml_visible` saved/restored across sessions like the playlist window
- **Fixed ML remove performance** — no longer re-queries the entire DB after every delete

### Architecture
- **Plugin manager** — loads visualizer and filetype plugins from configured directories at startup
- **Refactored build setup** — workspace with `src/` for core and `frontends/` for UI layers
- **32 new tests** — covering media library operations, duration cache, and EQ config
- **CLI agent rules** — `AGENTS.md` documents working conventions for AI-assisted development

---

- **GTK4 GUI** with automatic light/dark theme support and system accent color
- **Winamp-style transport controls** — z / x / c / v / b keyboard bindings
- **Scrolling marquee** showing current track title and artist
- **Seek bar** with scrub-before-play support
- **Volume slider**
- **Playlist window** — drag-to-reorder, per-track duration display, broken/missing file indicators
- **Jump-to-track** — press `j` to open a live search window, navigate with arrow keys, press Enter to jump and play
- **Repeat & shuffle** — cycle repeat (off / song / all) with `r`; toggle shuffle with `s`; clearly labelled `🔁1` / `🔁A`
- **Real-audio waveform visualizer** — center-line oscilloscope display driven by actual PCM audio data; 1–6 configurable color zones (zone 1 at bottom, highest at top); Lines or Filled style; fullscreen mode via `f` or double-click covers the entire display with keyboard passthrough (`z x c v b r s i j`) and status toasts; inspired by the [LSaO Visualizer](https://github.com/aaronfbianchi/LSaO-visualizer)
- **10-band equalizer** — built-in GStreamer EQ with 7 presets (Flat, Rock, Pop, Jazz, Classical, Bass Boost, Treble Boost); accessible via the EQ button or `u` key
- **EQ pre-amp** — 50 %–150 % gain slider above the EQ bands; only active when EQ is enabled
- **ID3 tag viewer/editor** — view and edit title, artist, album, year, track, genre, comment; access custom frames; press `d` in the TUI
- **Settings panel** — appearance (theme, custom skin), behaviour, visualizer mode, plugin directories
- **Background duration probing** — file lengths appear immediately on load without blocking the UI, with a persistent cache
- **Missing file detection** — files that disappear from disk are marked with a warning indicator
- **Duration cache** — probed durations persist to disk and appear instantly on next launch
- **Mini visualizer** — bars or waveform mode, with support for external plugin visualizers
- **Granite visualizer plugin** — a Geiss-inspired plasma animation (separate `.so` plugin) with configurable speed, palette, and feedback strength; press `f` or double-click the visualizer to run a fullscreen mode
- **Plugin framework (ABI v2)** — install/uninstall `.so` plugins at runtime; visualizer and filetype plugins; settings schema with per-plugin TOML persistence; live `on_setting_changed` callbacks; automatic v1 plugin shimming for backward compatibility
- **Media library** — SQLite-backed library with Files and Playlists tabs, full-text search, play-count tracking, folder watch/rescan, configurable periodic rescans
- **CLI / TUI mode** — a full terminal UI for headless or keyboard-only use (`sparkamp`)
- **Playlist persistence** — saves and restores the playlist and player position between sessions
- **Skin system** — light and dark CSS skins, switchable at runtime; user skins at `~/.config/sparkamp/skins/`
- **Format support** — MP3, FLAC, OGG Vorbis, Opus, WAV, M4A; filetype plugin API for additional formats
- **Flatpak packaging** — manifest and GitHub Actions workflow that builds and uploads a `.flatpak` bundle on every push to `main`

---

## Roadmap

These are the directions the project is heading, in no particular order. Nothing here is committed to a timeline.

- **Plugin Settings UI** — in-app settings panel for installed plugins (schema-driven widgets auto-generated from the plugin's declared settings)
- **Plugin install dialog** — browse and install `.so` files from within the app
- **Skin format and migration tool** — a new skin format with a migration path from classic Winamp `.wsz` skins
- **macOS support** — Milestone 1 complete: native SwiftUI player with full playback, playlist, background metadata scanning, broken-file detection, and CSS skin system. Milestone 2 in progress.
- **Equalizer UI polish** — save named custom presets, per-band labels in the GTK window
- **TUI media library** — browse/search the media library from the terminal UI
- **Confirmation when adding non-library files** — interstitial dialog when adding files that aren't in the ML

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
| Media library | SQLite via `rusqlite` (bundled, no system dep) |
| Plugin loading | `libloading` (dlopen) |
| CLI | Clap |
| macOS frontend | Swift / SwiftUI + Rust FFI staticlib |

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

Build the main binary and all plugins (workspace build):
```bash
cargo build --release
./target/release/sparkamp --ui      # GTK4 graphical interface
./target/release/sparkamp           # Terminal UI
```

**macOS (Xcode):**

Requires Xcode and GStreamer installed via Homebrew:
```bash
brew install gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad gst-plugins-ugly
```
Open `frontends/SparkampMac/SparkampMac.xcodeproj` and build. The Cargo build phase runs `cargo build -p sparkamp-macos` automatically and links the result as a static library.

Build and install just the Granite visualizer plugin:
```bash
cargo build --release -p viz_granite
# Copy the .so to your Sparkamp plugins directory or install via the app
cp target/release/libviz_granite.so ~/.local/share/sparkamp/plugins/dev.sparkamp.viz.granite/
```

For TUI mode:
```bash
./target/release/sparkamp
```

---

## Plugin System

Sparkamp supports third-party plugins as shared libraries (`.so` files on Linux).

### Plugin types

- **Visualizer plugins** — replace the built-in bars/oscilloscope with custom GPU or software rendering; can declare an optional `fullscreen` callback triggered by `f` or double-click
- **Filetype plugins** — add support for additional audio container formats (decoder + metadata reader)

### ABI v2

Every plugin exports one C function:
```c
const SparkPluginAbi *sparkamp_plugin(void);
```

The returned struct declares the plugin's identity, settings schema, and callbacks. See `src/plugin_abi.rs` for the full type definitions. A reference implementation is in `plugins/viz_granite/`.

### Settings

Plugins declare a null-terminated array of `SparkSettingDef` entries. Sparkamp reads the schema at load time, persists values to `~/.local/share/sparkamp/plugins/<plugin_id>/settings.toml`, and calls `on_setting_changed` whenever the user modifies a value.

### Backward compatibility

Plugins compiled against the v1 API (`sparkamp_viz_plugin` / `sparkamp_filetype_plugin` entry points) are automatically shimmed to the v2 interface at load time. No rebuild required.

---

## Keyboard Shortcuts

### Player window (GTK4)

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
| `a` | Cycle visualizer mode (built-in → plugins) |
| `f` | Fullscreen waveform visualizer (Waveform mode only; also opens on double-click) |
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

### TUI (terminal UI)

| Key | Action |
|---|---|
| `z` / `x` / `c` / `v` / `b` | Previous / Play / Pause / Stop / Next |
| `j` | Jump to track |
| `a` | Cycle visualizer mode |
| `f` | Fullscreen waveform (Waveform mode only) |
| `r` / `s` | Repeat / Shuffle |
| `u` | Open EQ overlay |
| `d` | ID3 tag editor |
| `e` | Settings overlay |
| `m` | Media library |
| `←` / `→` | Seek ±5 s |
| `[` / `]` | Volume ±5 % |
| `q` | Quit |

### Equalizer overlay (TUI)

| Key | Action |
|---|---|
| `←` / `→` | Select band |
| `↑` / `↓` | ±1 dB |
| `PgUp` / `PgDn` | ±3 dB |
| `[` / `]` | Pre-amp ±5 % |
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

[GNU Affero General Public License v3.0](https://www.gnu.org/licenses/agpl-3.0.html) (AGPL-3.0)
