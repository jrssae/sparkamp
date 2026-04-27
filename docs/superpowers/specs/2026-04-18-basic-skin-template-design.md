# Basic Skin Template Design

**Status:** Spec (pending implementation plan)
**Date:** 2026-04-18
**Scope:** Replace the current 30+ variable skin system with a minimal, platform-neutral 14-variable CSS template that controls colors, fonts, and font sizes for every Sparkamp window on Linux and macOS.

---

## 1. Goal

Ship a "basic" skin template: a single `.css` file declaring 14 variables in a `:root { }` block, which fully controls Sparkamp's appearance on both Linux (GTK4) and macOS. The same file renders identically on both platforms — no per-OS customization. An advanced template with fine-grained overrides will follow in a future iteration; this spec covers only the basic template.

The Appearance settings pane is restructured around a scrollable skin list with Add / Remove / Download / Export-how-to actions. The existing Theme dropdown, Accent color picker, and Custom-skin-name entry are removed.

## 2. The 14 variables

Grouped by purpose:

### Colors (5)

| Variable | Accepts | Controls |
|---|---|---|
| `--sp-background` | `#rgb` / `#rrggbb` | Window chrome of every window |
| `--sp-text-background` | `#rgb` / `#rrggbb` | Marquee panel, time display, playlist, media library, input fields |
| `--sp-text-color` | `#rgb` / `#rrggbb` | All body text sitting on `--sp-text-background` and on `--sp-background` |
| `--sp-highlight` | `#rgb` / `#rrggbb` | Selection bg, playing-row text, focus ring, toggle-on button bg, seek/volume fill |
| `--sp-broken-color` | `#rgb` / `#rrggbb` | Broken/missing track row text (playlist + ML) and the ✗ prefix glyph |

### Buttons (5)

| Variable | Accepts | Controls |
|---|---|---|
| `--sp-button-color` | `#rgb` / `#rrggbb` | Button background — resting state |
| `--sp-button-hover` | `#rgb` / `#rrggbb` | Button background — mouse over |
| `--sp-button-active` | `#rgb` / `#rrggbb` | Button background — toggled-on (shuffle / repeat / PL / Info) |
| `--sp-button-pressed` | `#rgb` / `#rrggbb` | Button background — momentary click (mouse held down) |
| `--sp-button-text-color` | `#rgb` / `#rrggbb` | Icon / label color on buttons in every state |

### Fonts (4)

| Variable | Accepts | Controls |
|---|---|---|
| `--sp-font-family` | CSS font-family string | All text except the time digits (which are always monospace) |
| `--sp-font-size` | `Npx` | Playlist, ML, settings, ID3 editor, Jump, Information, Keyboard Shortcuts, Dedupe, Artwork, Equalizer, all buttons, volume %, status bar |
| `--sp-font-size-large` | `Npx` | Time index display |
| `--sp-font-size-marquee` | `Npx` | Marquee title in the now-playing panel |

### Auto-derived (not user-facing)

These are computed in code; skin authors do not control them directly:

- Selected row background — `highlight` at 18% opacity over `text-background`
- Playing row background — `highlight` at 10% opacity over `text-background`
- Hover row background — `highlight` at 8% opacity over `text-background`
- Seek / volume track background — `text-background`
- Seek / volume fill + thumb — `highlight`
- Muted / dim text (duration column, volume %) — `text-color` at 60% opacity
- Border colors — `background` luminance ±8%
- Time-digit font — hardcoded monospace

### Button state semantics

The four button color variables map as follows:

- `color` — resting, no interaction
- `hover` — pointer is over the button
- `active` — the button's toggle is ON (only applies to `shuffle`, `repeat`, `PL`, `Info` mode buttons)
- `pressed` — the user is currently pressing/holding the button (momentary; applies to every button)

Text/icon sitting on the button uses `--sp-button-text-color` across all four states.

## 3. File format

A skin is one `.css` file with exactly one `:root { }` block declaring the 14 variables. Example:

```css
:root {
    --sp-background:         #1a1a1a;
    --sp-text-background:    #0c0c0c;
    --sp-text-color:         #cccccc;
    --sp-highlight:          #00ccff;
    --sp-broken-color:       #ff7700;

    --sp-button-color:       #212121;
    --sp-button-hover:       #2e2e2e;
    --sp-button-active:      #003e52;
    --sp-button-pressed:     #3a3a3a;
    --sp-button-text-color:  #aaaaaa;

    --sp-font-family:        "Inter, system-ui, sans-serif";
    --sp-font-size:          12px;
    --sp-font-size-large:    32px;
    --sp-font-size-marquee:  14px;
}
```

Parser contract:

- The parser reads only the first `:root { }` block.
- Unknown `--sp-*` variables are ignored.
- Missing variables fall back to the `Dark` built-in's defaults, so partial skin files still render.
- Malformed values (unparseable colors, sizes without `px`, etc.) fall back to the `Dark` default for that specific field; parsing does not fail overall.
- Comments inside `:root` are stripped before parsing.
- Anything outside `:root { }` is ignored (reserved for the future advanced template).

## 4. Built-in templates

Two built-in skins ship embedded in the binary, both expressed in the 14-variable format:

- **Dark** — Winamp-inspired cyan-on-near-black
- **Light** — clean light variant (off-white background, dark text, blue highlight)

Neither is special: they are the templates that the "Download skin…" button exports. The Dark values are also the fallback defaults used when a user skin omits or malforms a variable.

## 5. User skin directory and identity

- User skins live at `~/.config/sparkamp/skins/*.css`.
- A skin's identity is the lowercased filename stem (e.g. `~/.config/sparkamp/skins/midnight-teal.css` → `midnight-teal`).
- Built-ins are identified by the names `dark` and `light`.

## 6. Config changes

In `src/config.rs`, the `Appearance` section changes:

**Removed:**

- `theme: ThemeChoice` (enum `System` / `Dark` / `Light`)
- `accent_color: String`
- `custom_skin: String`

**Added:**

- `active_skin: String` — default `"dark"`. The currently applied skin, persisted across launches.
- `hidden_skins: Vec<String>` — default empty. Names of user skins that the user has pressed Remove on; the skin directory is still scanned, but hidden entries are filtered out.

Dark and Light cannot appear in `hidden_skins` — the Remove button is disabled for built-ins, and any pre-existing `"dark"` / `"light"` entries in the config are ignored.

Greenfield migration: on first launch after upgrade, an absent `active_skin` defaults to `"dark"`. No migration logic is required beyond that default, per the user's greenfield declaration.

## 7. Core architecture (`src/skin.rs`)

The `skin.rs` module is the single source of truth for parsing, built-ins, derivations, and rendering.

### 7.1 Public types

```rust
pub struct Skin {
    pub name: String,
    pub source: SkinSource,           // BuiltIn | UserFile(PathBuf)
    pub vars: SkinVars,
}

pub struct SkinVars {
    pub background: Rgb,
    pub text_background: Rgb,
    pub text_color: Rgb,
    pub highlight: Rgb,
    pub broken_color: Rgb,
    pub button_color: Rgb,
    pub button_hover: Rgb,
    pub button_active: Rgb,
    pub button_pressed: Rgb,
    pub button_text_color: Rgb,
    pub font_family: String,
    pub font_size: f32,
    pub font_size_large: f32,
    pub font_size_marquee: f32,
}

pub struct SkinEntry {
    pub name: String,           // "dark" | "light" | user stem
    pub display_name: String,   // "Dark" | "Light" | user stem capitalized
    pub is_builtin: bool,
    pub path: Option<PathBuf>,  // Some for user files, None for built-ins
}

pub struct Rgb { r: u8, g: u8, b: u8 }
impl Rgb {
    pub fn with_opacity(&self, alpha: f32) -> String; // → "rgba(r, g, b, alpha)"
}
```

### 7.2 Public functions

```rust
pub fn parse_skin_vars(css: &str) -> SkinVars;
pub fn load_skin(name: &str) -> Option<Skin>;
pub fn list_skins(hidden: &[String]) -> Vec<SkinEntry>;
pub fn add_user_skin(src: &Path) -> Result<SkinEntry, SkinError>;
pub fn render_gtk_css(vars: &SkinVars) -> String;
pub fn dark_template_css() -> &'static str;
pub fn light_template_css() -> &'static str;
pub fn skin_guide_md() -> &'static str;
```

- `list_skins` returns `[Dark, Light]` first, then alphabetically sorted user entries from `~/.config/sparkamp/skins/*.css`, with `hidden` names filtered out (except Dark/Light, which are never filtered).
- `add_user_skin` validates the source file parses, copies it to the skins dir with a uniquified filename if there's a collision, and returns the `SkinEntry`.
- `render_gtk_css` emits the complete GTK4 stylesheet: every widget-class rule with the variables' values (and derivations) inlined. This replaces today's static `style_dark.css` / `style_light.css`.

### 7.3 Embedded assets

Three compile-time string constants:

- `DARK_TEMPLATE_CSS: &str` — the 14-variable Dark template, also what `Download skin…` exports for Dark.
- `LIGHT_TEMPLATE_CSS: &str` — same for Light.
- `SKIN_GUIDE_MD: &str` — the how-to document, what `Export how-to guide…` writes out.

### 7.4 Removed

The existing `SPARKAMP_TO_GTK` translation table, `parse_sparkamp_vars` free-form hashmap API, `is_dark_skin` luminance-sniffing, `load_prepared`, accent-color injection in `prepare_css`, and the `--sparkamp-*` → GTK rule generator all go away. The new pipeline is: parse → `SkinVars` → `render_gtk_css` → `CssProvider`.

## 8. GTK frontend changes (`frontends/gtk/`)

- **Delete** `style.css`, `style_dark.css`, `style_light.css`. The GTK stylesheet is 100% generated at runtime.
- In `window.rs`, the startup CSS load path calls `skin::render_gtk_css(&active_skin.vars)` and passes the string to `CssProvider::load_from_data`. No `include_str!` of CSS files.
- On skin change (user clicks a row in the Appearance list): rebuild the CSS string and swap it into the existing `CssProvider`. No window rebuild.
- **Appearance tab replacement.** The current Theme dropdown (rows ~5740–5810 in `window.rs`), Accent color picker (~5817–5950), and Custom-skin-name entry (~5988–6006) are removed. In their place:
  - A `ScrolledWindow` containing a `ListBox` populated from `list_skins(&hidden)`. Each row shows the display name and a "(built-in)" tag for Dark/Light. The currently active row is marked (e.g. bold text + a dot).
  - Row selection triggers live skin application.
  - Below the list, a horizontal `Box` with three buttons: `Add skin…`, `Remove`, `Download skin…`.
  - A separator, then a `Documentation` group with one button: `Export how-to guide…`.
- The `resolve_accent_hex` helper and all accent-injection plumbing are deleted.

## 9. macOS frontend changes (`frontends/SparkampMac/`)

### 9.1 Theme.swift

- `SkinTheme` is refactored from ~34 stored `Color` fields to the 14-variable shape: 10 `Color`s, a `String` for family, and three `CGFloat`s for sizes.
- Derivations (row backgrounds, dim text, border color, monospace-for-time) become computed properties on `SkinTheme`.
- `CSSParser` is simplified to read only the 14 `--sp-*` variables and to parse `font-family` / `Npx` values.
- Button-image override parsing is removed from the basic template (deferred to the advanced template).
- `ThemeManager` API changes:
  - **Removed:** `useDark`, `useLight`, `useSystem`, `setAccentColor`, `openSkinPicker`, `removeCustomSkin`, `exportDefaultCSS`.
  - **Added:** `setActiveSkin(name: String)`, `addUserSkin(url: URL)`, `hideSkin(name: String)`, `exportSkin(name: String, to: URL)`, `exportGuide(to: URL)`, `listSkins() -> [SkinEntry]`.

### 9.2 Font propagation

Every SwiftUI file that hardcodes `.font(.system(size: N))` is rewritten to read from the theme. The root view sets `.font(theme.body)` as a default; views that need the marquee or large size override locally.

Files to touch (fonts and/or explicit colors):

1. `ContentView.swift`
2. `MarqueeView.swift`
3. `PlayerWindow.swift`
4. `PlaylistView.swift`
5. `MediaLibraryWindow.swift`
6. `Id3EditorWindow.swift`
7. `JumpToTrackView.swift`
8. `KeyboardShortcutsView.swift`
9. `DeduplicatorWindow.swift`
10. `ArtworkWindow.swift`
11. `SettingsWindow.swift`
12. `EqualizerWindow.swift`
13. `FullscreenVisualizerWindow.swift` — only where it shows text overlays; visualizer graphics are plugin-controlled and not skinned here.
14. `VisualizerView.swift` — same as above.

### 9.3 SettingsWindow.swift — AppearancePane

Replaced entirely. New body:

```swift
Form {
    Section("Skin") {
        List(selection: $selectedSkin) {
            ForEach(themeManager.listSkins(), id: \.name) { entry in
                HStack {
                    Text(entry.displayName)
                    if entry.isBuiltin {
                        Text("(built-in)").foregroundStyle(.secondary)
                    }
                    Spacer()
                    if entry.name == themeManager.activeSkin {
                        Image(systemName: "checkmark.circle.fill")
                    }
                }
                .tag(entry.name)
            }
        }
        .onChange(of: selectedSkin) { _, new in
            if let new { themeManager.setActiveSkin(new) }
        }

        HStack {
            Button("Add skin…")       { addSkinPicker() }
            Button("Remove")          { removeSelected() }
                .disabled(isBuiltin(selectedSkin))
            Button("Download skin…")  { downloadSelected() }
        }
    }

    Section("Documentation") {
        Button("Export how-to guide…") { exportGuide() }
    }
}
.formStyle(.grouped)
```

### 9.4 FFI (`frontends/macos/src/lib.rs` + `sparkamp_bridge.h`)

Swift reads the skins directory directly using `FileManager` and calls into Rust only for parsing (`parse_skin_vars`). No new FFI functions are required for list / add / hide / export operations — those are native Swift file operations. This keeps the FFI surface minimal.

## 10. Appearance pane UX (both frontends)

```
┌─────────────────────────────────────────────┐
│ Skin                                        │
│ ┌─────────────────────────────────────────┐ │
│ │ Dark             (built-in)      ● Active│ │
│ │ Light            (built-in)              │ │
│ │ Midnight Teal                            │ │
│ │ Retro Amber                              │ │
│ └─────────────────────────────────────────┘ │
│                                             │
│ [ Add skin… ]  [ Remove ]  [ Download skin…]│
│                                             │
│ ─────────────────────────────────────────── │
│                                             │
│ Documentation                               │
│ [ Export how-to guide… ]                    │
└─────────────────────────────────────────────┘
```

### Interactions

- **Click a row** — loads and applies immediately; persists `active_skin` to config.
- **Add skin…** — file picker (`.css` only) → parse-validates → copies into `~/.config/sparkamp/skins/` with uniquified filename on collision → list refreshes → newly added skin is selected and applied. If parsing fails, the file is not copied and an error alert is shown.
- **Remove** — appends the selected skin's name to `hidden_skins` in config. The CSS file on disk is not touched. Disabled when the selected skin is Dark or Light. Selection falls back to the previously active skin (or Dark if the removed skin was active).
- **Download skin…** — save dialog → writes the selected skin's CSS to the chosen destination. For built-ins, writes the embedded template; for user skins, copies the source file.
- **Export how-to guide…** — save dialog → writes `SKIN_GUIDE_MD` as `sparkamp-skin-guide.md` by default.

## 11. How-to document (`SKIN_GUIDE_MD`)

Bundled content (full outline — exact prose is written during implementation):

- **What a skin is** — one paragraph: one `.css` file, 14 values in a `:root { }` block, works identically on Linux and macOS.
- **Creating your first skin** — step-by-step:
  1. Export a template (Dark or Light) from Settings → Appearance.
  2. Open the `.css` file in a text editor.
  3. Edit the values inside `:root { }`.
  4. Save, then use Settings → Appearance → Add skin… to load it.
- **The 14 variables — reference.** For each variable: name, accepted value format, one-line description, and a bulleted list of every UI element it controls (as in §2).
- **Button state semantics** — resting / hover / active (toggled on) / pressed (momentary).
- **Auto-derived values (not skinnable)** — the list from §2 "Auto-derived".
- **Tips** — contrast between `text-color` and `text-background`, `highlight` should be distinct from both, button-state colors should be perceptually ordered.
- **Limits** — no structural changes (paddings/sizes/corner radii), no images. Future advanced template will cover these.

## 12. Testing strategy

### 12.1 Rust core unit tests (`src/skin.rs`)

- `parse_skin_vars` — happy path: valid `:root` block with all 14 vars produces the correct `SkinVars`.
- `parse_skin_vars` — missing variables fall back to the Dark defaults for each absent field.
- `parse_skin_vars` — unknown `--sp-*` entries are ignored, no error.
- `parse_skin_vars` — malformed color (`#zzz`, empty, numeric overflow) falls back to the Dark default for that field.
- `parse_skin_vars` — malformed size (`abc`, size without `px`) falls back to the Dark default.
- `parse_skin_vars` — comments inside `:root` are stripped.
- `DARK_TEMPLATE_CSS` and `LIGHT_TEMPLATE_CSS` each parse cleanly and produce valid `SkinVars`.
- `render_gtk_css(&vars)` — output contains one selector line per expected widget class and substitutes every variable. A snapshot-test variant compares against a reference string for the Dark built-in.
- Derivations — `with_opacity(0.18)` on highlight produces the correct rgba string; luminance-based border derivation yields a color with a plausible delta.
- `list_skins(&[])` — returns `[Dark, Light]` with no user files; with user files it returns them alphabetically after the built-ins.
- `list_skins(&[...])` — user entries in `hidden` are filtered out; `"dark"` / `"light"` in `hidden` are ignored (built-ins are always returned).
- `add_user_skin` — valid file copies into skins dir and returns `SkinEntry`; filename collision produces a uniquified name.
- `add_user_skin` — invalid CSS (no `:root`, parse-failing content) returns an error and does not copy.

### 12.2 GTK frontend smoke tests

- Skin list populates on startup with both built-ins.
- Clicking a row in the list updates the active skin in state and the `CssProvider` is swapped.
- Remove button is disabled when Dark or Light is selected; enabled for user skins.

Run under the existing GTK test harness already used for other window tests.

### 12.3 macOS frontend tests (XCTest)

- `SkinTheme.parse(css:)` produces the same `SkinVars` values as the Rust parser for a shared test-vector file committed to `testdata/`.
- `ThemeManager.setActiveSkin("dark")` sets `currentTheme` to the Dark values.
- `AppearancePane` renders the skin list; programmatically selecting a row updates `themeManager.activeSkin`.

### 12.4 Manual QA checklist

- All 8 secondary windows (Information, Jump, ID3, Settings, Dedupe, Artwork, Keyboard Shortcuts, Equalizer) render with the active skin's colors and fonts on both platforms.
- Export template CSS → re-import via Add → appears in list and renders identically.
- Export how-to guide → file opens cleanly in a Markdown viewer.
- Time digits remain monospace regardless of `--sp-font-family`.
- Broken-track rows show the ✗ prefix rendered in `--sp-broken-color`.

### 12.5 Build/CI gate

`cargo build && cargo test` on Linux with zero warnings and zero failures (per `CLAUDE.md`). Xcode test scheme passes on macOS.

## 13. Out of scope

Explicitly deferred to the future "advanced" skin template:

- Button image overrides (prev / play / pause / stop / next PNGs in a zip bundle).
- Fine-grained per-element color overrides beyond the 14 variables.
- Structural changes (padding, margins, corner radii, window sizes).
- Custom fonts embedded in a skin package (`@font-face`).
- Per-window or per-element font-size overrides.
- Additional state-indicator colors (destructive buttons, error labels, validation failures).
- Zip-file skin packaging with CSS + images + metadata.

## 14. Summary of files touched

- **Core:** `src/skin.rs` (heavy refactor), `src/config.rs` (field removals + additions).
- **GTK:** `frontends/gtk/window.rs` (appearance tab rewrite, startup CSS path change). Deletes: `frontends/gtk/style.css`, `style_dark.css`, `style_light.css`.
- **macOS:** `frontends/SparkampMac/Sources/Theme.swift` (heavy refactor), `SettingsWindow.swift` (AppearancePane rewrite + font propagation), + 13 other SwiftUI files (font propagation only). No FFI surface changes.
