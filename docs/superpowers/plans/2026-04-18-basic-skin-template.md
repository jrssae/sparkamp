# Basic Skin Template Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> **Commit policy:** Per the user's request, this plan contains NO `git commit` steps. After implementing tasks and verifying builds + tests, hand off to the user for manual QA on Linux before any commits. Do not run `git commit` during execution.

**Goal:** Replace Sparkamp's 30+ variable skin system with a 14-variable CSS template that drives colors, fonts, and sizes identically on Linux (GTK4) and macOS.

**Architecture:** A single Rust `SkinVars` struct parsed from a `:root { }` block is the source of truth. GTK CSS is generated at runtime by `render_gtk_css(&SkinVars)` (replacing the three static stylesheets). macOS mirrors the struct in Swift and applies it through `ThemeManager`. The Appearance pane on both platforms becomes a scrollable skin list with Add / Remove / Download / Export-guide actions; the Theme dropdown, Accent color picker, and Custom-skin-name entry are removed.

**Tech Stack:** Rust (core + GTK4 frontend), Swift/SwiftUI (macOS frontend), CSS (skin format).

**Reference:** Full design at `docs/superpowers/specs/2026-04-18-basic-skin-template-design.md`.

---

## File Structure

### Created

- `docs/superpowers/plans/2026-04-18-basic-skin-template.md` — this plan (already present once created).

### Modified (heavy)

- `src/skin.rs` — replaces free-form variable parsing with fixed `SkinVars` struct; adds `render_gtk_css`, `list_skins`, `add_user_skin`, new embedded templates and skin guide.
- `src/config.rs` — removes `theme`, `accent_color`, `custom_skin`; adds `active_skin`. (`hidden_skins` already exists.)
- `frontends/gtk/window.rs` — removes startup accent/theme resolution, switches to `render_gtk_css`, rewrites the Appearance tab body.
- `frontends/SparkampMac/Sources/Theme.swift` — collapses `SkinTheme` to 14 variables with computed derivations; rewrites `CSSParser`; rewrites `ThemeManager` API.
- `frontends/SparkampMac/Sources/SettingsWindow.swift` — rewrites `AppearancePane`; sweeps its own font hardcodes.

### Modified (font propagation only)

- `frontends/SparkampMac/Sources/ContentView.swift`
- `frontends/SparkampMac/Sources/MarqueeView.swift`
- `frontends/SparkampMac/Sources/PlayerWindow.swift`
- `frontends/SparkampMac/Sources/PlaylistView.swift`
- `frontends/SparkampMac/Sources/MediaLibraryWindow.swift`
- `frontends/SparkampMac/Sources/Id3EditorWindow.swift`
- `frontends/SparkampMac/Sources/JumpToTrackView.swift`
- `frontends/SparkampMac/Sources/KeyboardShortcutsView.swift`
- `frontends/SparkampMac/Sources/DeduplicatorWindow.swift`
- `frontends/SparkampMac/Sources/ArtworkWindow.swift`
- `frontends/SparkampMac/Sources/EqualizerWindow.swift`
- `frontends/SparkampMac/Sources/FullscreenVisualizerWindow.swift`
- `frontends/SparkampMac/Sources/VisualizerView.swift`

### Deleted

- `frontends/gtk/style.css`
- `frontends/gtk/style_dark.css`
- `frontends/gtk/style_light.css`

---

## Phased Ordering

- **Phase 1** (Tasks 1–11) — Rust core: types, parser, built-ins, GTK renderer, config. Full unit-test coverage. Compiles and tests from Linux alone.
- **Phase 2** (Tasks 12–16) — GTK integration: wire the new renderer, delete old stylesheets, rewrite Appearance tab. Verified with `cargo build` and manual GTK launch.
- **Phase 3** (Tasks 17–21) — macOS core: `Theme.swift` refactor + `AppearancePane` rewrite. Cannot be built from Linux; source-diff-level review only.
- **Phase 4** (Tasks 22–27) — macOS font propagation sweep across 13 SwiftUI files. Mechanical, same pattern each file.
- **Phase 5** (Task 28) — Cross-platform final verification.

After Phase 2 the Linux build is fully functional and the user can manual-QA before proceeding to Phase 3+.

---

## Phase 1 — Rust Core

### Task 1: Rgb type

**Files:**
- Modify: `src/skin.rs` (top of file, replacing existing helpers)
- Test: `src/skin.rs` (in the existing `#[cfg(test)] mod tests` block)

- [ ] **Step 1: Write failing tests**

Add to the tests module in `src/skin.rs`:

```rust
#[test]
fn rgb_parse_six_digit_hex() {
    let c = Rgb::parse("#1a2b3c").unwrap();
    assert_eq!((c.r, c.g, c.b), (0x1a, 0x2b, 0x3c));
}

#[test]
fn rgb_parse_three_digit_hex() {
    let c = Rgb::parse("#abc").unwrap();
    assert_eq!((c.r, c.g, c.b), (0xaa, 0xbb, 0xcc));
}

#[test]
fn rgb_parse_accepts_whitespace() {
    assert!(Rgb::parse("  #ff0000  ").is_some());
}

#[test]
fn rgb_parse_rejects_invalid() {
    assert!(Rgb::parse("#zzz").is_none());
    assert!(Rgb::parse("#12345").is_none()); // 5 digits
    assert!(Rgb::parse("").is_none());
    assert!(Rgb::parse("red").is_none());
}

#[test]
fn rgb_to_hex() {
    let c = Rgb { r: 0x1a, g: 0x2b, b: 0x3c };
    assert_eq!(c.to_hex(), "#1a2b3c");
}

#[test]
fn rgb_with_opacity_formats_rgba() {
    let c = Rgb { r: 255, g: 128, b: 0 };
    assert_eq!(c.with_opacity(0.5), "rgba(255, 128, 0, 0.5)");
}
```

- [ ] **Step 2: Run tests to verify they fail**

```
cargo test --lib skin::tests::rgb_
```

Expected: compile errors for missing `Rgb` type.

- [ ] **Step 3: Implement `Rgb`**

Near the top of `src/skin.rs`, above the `Skin` struct:

```rust
/// An opaque 24-bit sRGB color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    /// Parse `#rgb` or `#rrggbb`. Leading/trailing whitespace is tolerated.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let hex = s.strip_prefix('#')?;
        match hex.len() {
            3 => {
                let r = u8::from_str_radix(&hex[0..1].repeat(2), 16).ok()?;
                let g = u8::from_str_radix(&hex[1..2].repeat(2), 16).ok()?;
                let b = u8::from_str_radix(&hex[2..3].repeat(2), 16).ok()?;
                Some(Rgb { r, g, b })
            }
            6 => {
                let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
                Some(Rgb { r, g, b })
            }
            _ => None,
        }
    }

    /// Render as `#rrggbb`.
    pub fn to_hex(&self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// Render as `rgba(r, g, b, alpha)` for GTK CSS.
    pub fn with_opacity(&self, alpha: f32) -> String {
        format!("rgba({}, {}, {}, {})", self.r, self.g, self.b, alpha)
    }

    /// Relative luminance per ITU-R BT.709 in linear-sRGB space.
    pub fn luminance(&self) -> f32 {
        fn lin(c: u8) -> f32 {
            let s = c as f32 / 255.0;
            if s <= 0.04045 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
        }
        0.2126 * lin(self.r) + 0.7152 * lin(self.g) + 0.0722 * lin(self.b)
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```
cargo test --lib skin::tests::rgb_
```

Expected: all `rgb_*` tests pass. No warnings from the new code.

---

### Task 2: SkinVars struct with Dark defaults

**Files:**
- Modify: `src/skin.rs`

- [ ] **Step 1: Write failing tests**

Append to the tests module:

```rust
#[test]
fn skin_vars_default_is_dark() {
    let v = SkinVars::dark_defaults();
    assert_eq!(v.background.to_hex(), "#1a1a1a");
    assert_eq!(v.text_background.to_hex(), "#0c0c0c");
    assert_eq!(v.text_color.to_hex(), "#cccccc");
    assert_eq!(v.highlight.to_hex(), "#00ccff");
    assert_eq!(v.broken_color.to_hex(), "#ff7700");
    assert_eq!(v.button_color.to_hex(), "#212121");
    assert_eq!(v.button_hover.to_hex(), "#2e2e2e");
    assert_eq!(v.button_active.to_hex(), "#003e52");
    assert_eq!(v.button_pressed.to_hex(), "#3a3a3a");
    assert_eq!(v.button_text_color.to_hex(), "#aaaaaa");
    assert_eq!(v.font_family, "Inter, system-ui, sans-serif");
    assert_eq!(v.font_size, 12.0);
    assert_eq!(v.font_size_large, 32.0);
    assert_eq!(v.font_size_marquee, 14.0);
}

#[test]
fn skin_vars_light_defaults() {
    let v = SkinVars::light_defaults();
    assert_eq!(v.background.to_hex(), "#ededed");
    assert_eq!(v.text_color.to_hex(), "#222222");
    assert_eq!(v.highlight.to_hex(), "#1a6fc2");
}
```

- [ ] **Step 2: Run tests to verify they fail**

```
cargo test --lib skin::tests::skin_vars_
```

Expected: compile errors for missing `SkinVars`.

- [ ] **Step 3: Implement `SkinVars`**

Add to `src/skin.rs` after `Rgb`:

```rust
/// The 14 skin variables, fully resolved.
#[derive(Debug, Clone)]
pub struct SkinVars {
    pub background:        Rgb,
    pub text_background:   Rgb,
    pub text_color:        Rgb,
    pub highlight:         Rgb,
    pub broken_color:      Rgb,

    pub button_color:      Rgb,
    pub button_hover:      Rgb,
    pub button_active:     Rgb,
    pub button_pressed:    Rgb,
    pub button_text_color: Rgb,

    pub font_family:       String,
    pub font_size:         f32,
    pub font_size_large:   f32,
    pub font_size_marquee: f32,
}

impl SkinVars {
    /// Built-in Dark defaults. These also serve as fallback values when a
    /// user skin omits or malforms a variable.
    pub fn dark_defaults() -> Self {
        Self {
            background:        Rgb { r: 0x1a, g: 0x1a, b: 0x1a },
            text_background:   Rgb { r: 0x0c, g: 0x0c, b: 0x0c },
            text_color:        Rgb { r: 0xcc, g: 0xcc, b: 0xcc },
            highlight:         Rgb { r: 0x00, g: 0xcc, b: 0xff },
            broken_color:      Rgb { r: 0xff, g: 0x77, b: 0x00 },

            button_color:      Rgb { r: 0x21, g: 0x21, b: 0x21 },
            button_hover:      Rgb { r: 0x2e, g: 0x2e, b: 0x2e },
            button_active:     Rgb { r: 0x00, g: 0x3e, b: 0x52 },
            button_pressed:    Rgb { r: 0x3a, g: 0x3a, b: 0x3a },
            button_text_color: Rgb { r: 0xaa, g: 0xaa, b: 0xaa },

            font_family:       "Inter, system-ui, sans-serif".to_string(),
            font_size:         12.0,
            font_size_large:   32.0,
            font_size_marquee: 14.0,
        }
    }

    /// Built-in Light defaults.
    pub fn light_defaults() -> Self {
        Self {
            background:        Rgb { r: 0xed, g: 0xed, b: 0xed },
            text_background:   Rgb { r: 0xf6, g: 0xf6, b: 0xf6 },
            text_color:        Rgb { r: 0x22, g: 0x22, b: 0x22 },
            highlight:         Rgb { r: 0x1a, g: 0x6f, b: 0xc2 },
            broken_color:      Rgb { r: 0xcc, g: 0x55, b: 0x00 },

            button_color:      Rgb { r: 0xdc, g: 0xdc, b: 0xdc },
            button_hover:      Rgb { r: 0xcc, g: 0xcc, b: 0xcc },
            button_active:     Rgb { r: 0xcc, g: 0xe5, b: 0xf7 },
            button_pressed:    Rgb { r: 0xbb, g: 0xbb, b: 0xbb },
            button_text_color: Rgb { r: 0x33, g: 0x33, b: 0x33 },

            font_family:       "Inter, system-ui, sans-serif".to_string(),
            font_size:         12.0,
            font_size_large:   32.0,
            font_size_marquee: 14.0,
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```
cargo test --lib skin::tests::skin_vars_
```

Expected: pass.

---

### Task 3: parse_skin_vars — happy path

**Files:**
- Modify: `src/skin.rs`

- [ ] **Step 1: Write failing tests**

Append to the tests module:

```rust
#[test]
fn parse_skin_vars_all_fields() {
    let css = r#"
    :root {
        --sp-background:         #111111;
        --sp-text-background:    #222222;
        --sp-text-color:         #333333;
        --sp-highlight:          #444444;
        --sp-broken-color:       #555555;
        --sp-button-color:       #666666;
        --sp-button-hover:       #777777;
        --sp-button-active:      #888888;
        --sp-button-pressed:     #999999;
        --sp-button-text-color:  #aaaaaa;
        --sp-font-family:        "Helvetica, sans-serif";
        --sp-font-size:          13px;
        --sp-font-size-large:    40px;
        --sp-font-size-marquee:  18px;
    }
    "#;
    let v = parse_skin_vars(css);
    assert_eq!(v.background.to_hex(), "#111111");
    assert_eq!(v.text_background.to_hex(), "#222222");
    assert_eq!(v.text_color.to_hex(), "#333333");
    assert_eq!(v.highlight.to_hex(), "#444444");
    assert_eq!(v.broken_color.to_hex(), "#555555");
    assert_eq!(v.button_color.to_hex(), "#666666");
    assert_eq!(v.button_hover.to_hex(), "#777777");
    assert_eq!(v.button_active.to_hex(), "#888888");
    assert_eq!(v.button_pressed.to_hex(), "#999999");
    assert_eq!(v.button_text_color.to_hex(), "#aaaaaa");
    assert_eq!(v.font_family, "Helvetica, sans-serif");
    assert_eq!(v.font_size, 13.0);
    assert_eq!(v.font_size_large, 40.0);
    assert_eq!(v.font_size_marquee, 18.0);
}

#[test]
fn parse_skin_vars_strips_quotes_from_font_family() {
    let css = r#":root { --sp-font-family: "Inter"; }"#;
    let v = parse_skin_vars(css);
    assert_eq!(v.font_family, "Inter");
}

#[test]
fn parse_skin_vars_accepts_font_family_without_quotes() {
    let css = r#":root { --sp-font-family: monospace; }"#;
    let v = parse_skin_vars(css);
    assert_eq!(v.font_family, "monospace");
}
```

- [ ] **Step 2: Run tests to verify they fail**

```
cargo test --lib skin::tests::parse_skin_vars_
```

Expected: compile errors for missing `parse_skin_vars`.

- [ ] **Step 3: Implement parser**

Add to `src/skin.rs` (replacing the old `parse_sparkamp_vars`):

```rust
/// Parse `css` for a `:root { --sp-*: ...; }` block and produce a `SkinVars`.
///
/// Missing, unknown, or malformed variables fall back to Dark defaults
/// per-field. The parser is deliberately permissive: it never returns an
/// error; a completely empty or malformed input yields `SkinVars::dark_defaults()`.
pub fn parse_skin_vars(css: &str) -> SkinVars {
    let mut out = SkinVars::dark_defaults();
    let stripped = strip_css_comments(css);
    let Some(block) = extract_root_block(&stripped) else {
        return out;
    };

    for stmt in block.split(';') {
        let stmt = stmt.trim();
        if !stmt.starts_with("--sp-") { continue; }
        let Some(colon) = stmt.find(':') else { continue };
        let key = stmt[..colon].trim();
        let val = stmt[colon + 1..].trim();
        if key.is_empty() || val.is_empty() { continue; }
        apply_var(&mut out, key, val);
    }
    out
}

fn extract_root_block(css: &str) -> Option<String> {
    let idx = css.find(":root")?;
    let after = &css[idx + 5..];
    let open_rel = after.find('{')?;
    let close_rel = after[open_rel + 1..].find('}')?;
    Some(after[open_rel + 1..open_rel + 1 + close_rel].to_string())
}

fn apply_var(v: &mut SkinVars, key: &str, raw: &str) {
    match key {
        "--sp-background"        => if let Some(c) = Rgb::parse(raw) { v.background = c },
        "--sp-text-background"   => if let Some(c) = Rgb::parse(raw) { v.text_background = c },
        "--sp-text-color"        => if let Some(c) = Rgb::parse(raw) { v.text_color = c },
        "--sp-highlight"         => if let Some(c) = Rgb::parse(raw) { v.highlight = c },
        "--sp-broken-color"      => if let Some(c) = Rgb::parse(raw) { v.broken_color = c },
        "--sp-button-color"      => if let Some(c) = Rgb::parse(raw) { v.button_color = c },
        "--sp-button-hover"      => if let Some(c) = Rgb::parse(raw) { v.button_hover = c },
        "--sp-button-active"     => if let Some(c) = Rgb::parse(raw) { v.button_active = c },
        "--sp-button-pressed"    => if let Some(c) = Rgb::parse(raw) { v.button_pressed = c },
        "--sp-button-text-color" => if let Some(c) = Rgb::parse(raw) { v.button_text_color = c },
        "--sp-font-family"       => v.font_family = parse_font_family(raw),
        "--sp-font-size"         => if let Some(n) = parse_px(raw) { v.font_size = n },
        "--sp-font-size-large"   => if let Some(n) = parse_px(raw) { v.font_size_large = n },
        "--sp-font-size-marquee" => if let Some(n) = parse_px(raw) { v.font_size_marquee = n },
        _ => {} // unknown --sp-* variable — ignore
    }
}

fn parse_font_family(raw: &str) -> String {
    let t = raw.trim();
    if (t.starts_with('"') && t.ends_with('"')) || (t.starts_with('\'') && t.ends_with('\'')) {
        t[1..t.len()-1].to_string()
    } else {
        t.to_string()
    }
}

fn parse_px(raw: &str) -> Option<f32> {
    let t = raw.trim();
    let num_part = t.strip_suffix("px").unwrap_or(t).trim();
    num_part.parse::<f32>().ok()
}
```

Keep the existing `strip_css_comments` helper from the current `src/skin.rs`.

- [ ] **Step 4: Run tests to verify they pass**

```
cargo test --lib skin::tests::parse_skin_vars_
```

Expected: pass.

---

### Task 4: parse_skin_vars — fallbacks and edge cases

**Files:**
- Modify: `src/skin.rs` (tests only)

- [ ] **Step 1: Write failing tests**

Append:

```rust
#[test]
fn parse_skin_vars_missing_vars_fall_back_to_dark() {
    let css = r#":root { --sp-background: #111111; }"#;
    let v = parse_skin_vars(css);
    assert_eq!(v.background.to_hex(), "#111111");
    // Others come from Dark defaults
    assert_eq!(v.text_color.to_hex(), "#cccccc");
    assert_eq!(v.font_size, 12.0);
}

#[test]
fn parse_skin_vars_unknown_var_is_ignored() {
    let css = r#":root {
        --sp-background: #111111;
        --sp-not-a-real-var: #ff0000;
    }"#;
    let v = parse_skin_vars(css);
    assert_eq!(v.background.to_hex(), "#111111");
}

#[test]
fn parse_skin_vars_malformed_color_falls_back() {
    let css = r#":root {
        --sp-background: not-a-color;
        --sp-text-color: #ffffff;
    }"#;
    let v = parse_skin_vars(css);
    // background kept its Dark default
    assert_eq!(v.background.to_hex(), "#1a1a1a");
    // text-color applied
    assert_eq!(v.text_color.to_hex(), "#ffffff");
}

#[test]
fn parse_skin_vars_malformed_size_falls_back() {
    let css = r#":root { --sp-font-size: abc; }"#;
    let v = parse_skin_vars(css);
    assert_eq!(v.font_size, 12.0);
}

#[test]
fn parse_skin_vars_size_without_px_still_parses() {
    let css = r#":root { --sp-font-size: 15; }"#;
    let v = parse_skin_vars(css);
    assert_eq!(v.font_size, 15.0);
}

#[test]
fn parse_skin_vars_no_root_block_returns_defaults() {
    let css = "body { color: red; }";
    let v = parse_skin_vars(css);
    assert_eq!(v.background.to_hex(), "#1a1a1a");
}

#[test]
fn parse_skin_vars_strips_comments() {
    let css = r#":root {
        /* comment with : inside */
        --sp-background: #aaaaaa;
        /* trailing */
    }"#;
    let v = parse_skin_vars(css);
    assert_eq!(v.background.to_hex(), "#aaaaaa");
}

#[test]
fn parse_skin_vars_empty_string_returns_defaults() {
    let v = parse_skin_vars("");
    assert_eq!(v.background.to_hex(), "#1a1a1a");
}
```

- [ ] **Step 2: Run tests to verify they pass**

```
cargo test --lib skin::tests::parse_skin_vars_
```

Expected: all pass — the implementation from Task 3 already handles these.

- [ ] **Step 3: If any fail, fix in `src/skin.rs`**

The permissive parser from Task 3 should cover all these cases. If a failure surfaces (e.g. comment inside `:root` not stripped), adjust `strip_css_comments` / `extract_root_block` to handle it and rerun.

- [ ] **Step 4: Confirm whole `skin::tests` module still green**

```
cargo test --lib skin::tests
```

Expected: all pass.

---

### Task 5: SkinEntry and SkinSource types

**Files:**
- Modify: `src/skin.rs`

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn skin_entry_builtin_has_no_path() {
    let e = SkinEntry::builtin("dark", "Dark");
    assert_eq!(e.name, "dark");
    assert_eq!(e.display_name, "Dark");
    assert!(e.is_builtin);
    assert!(e.path.is_none());
}

#[test]
fn skin_entry_user_carries_path() {
    let p = std::path::PathBuf::from("/tmp/mine.css");
    let e = SkinEntry::user("mine", p.clone());
    assert_eq!(e.name, "mine");
    assert_eq!(e.display_name, "Mine");
    assert!(!e.is_builtin);
    assert_eq!(e.path, Some(p));
}
```

- [ ] **Step 2: Run test to verify it fails**

```
cargo test --lib skin::tests::skin_entry_
```

Expected: compile errors.

- [ ] **Step 3: Implement types**

Replace the existing `SkinSource` and `Skin` types in `src/skin.rs` with:

```rust
/// Origin of a loaded skin.
#[derive(Debug, Clone)]
pub enum SkinSource {
    BuiltIn,
    UserFile(PathBuf),
}

/// A fully-loaded skin.
#[derive(Debug, Clone)]
pub struct Skin {
    pub name: String,
    pub source: SkinSource,
    pub vars: SkinVars,
}

/// A listed skin in the Appearance pane. Lightweight — does not carry vars.
#[derive(Debug, Clone)]
pub struct SkinEntry {
    pub name: String,
    pub display_name: String,
    pub is_builtin: bool,
    pub path: Option<PathBuf>,
}

impl SkinEntry {
    pub fn builtin(name: &str, display_name: &str) -> Self {
        Self {
            name: name.to_string(),
            display_name: display_name.to_string(),
            is_builtin: true,
            path: None,
        }
    }

    pub fn user(stem: &str, path: PathBuf) -> Self {
        let display_name = titlecase(stem);
        Self {
            name: stem.to_string(),
            display_name,
            is_builtin: false,
            path: Some(path),
        }
    }
}

fn titlecase(stem: &str) -> String {
    // "midnight-teal" → "Midnight Teal"
    stem.split(|c: char| c == '-' || c == '_')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
```

- [ ] **Step 4: Run tests to verify they pass**

```
cargo test --lib skin::tests::skin_entry_
```

Expected: pass.

---

### Task 6: Embed built-in templates and skin guide

**Files:**
- Create: `frontends/gtk/style_dark.css` replaced with new 14-variable template (temporarily — will be deleted in Task 13)
  - **Note:** Actually create the new template files as `src/skin_templates/dark.css`, `src/skin_templates/light.css`, and `src/skin_templates/skin-guide.md`. This keeps skin assets grouped.
- Create: `src/skin_templates/dark.css`
- Create: `src/skin_templates/light.css`
- Create: `src/skin_templates/skin-guide.md`
- Modify: `src/skin.rs`

- [ ] **Step 1: Create the Dark template file**

`src/skin_templates/dark.css`:

```css
/* Sparkamp Dark — Basic Skin Template
 *
 * Edit these 14 values and save this file to
 * ~/.config/sparkamp/skins/<name>.css, then load it from
 * Settings → Appearance → Add skin…
 *
 * See the bundled skin-guide.md (Export how-to guide…) for a full
 * reference of which UI elements each variable controls.
 */
:root {
    /* Colors */
    --sp-background:         #1a1a1a;
    --sp-text-background:    #0c0c0c;
    --sp-text-color:         #cccccc;
    --sp-highlight:          #00ccff;
    --sp-broken-color:       #ff7700;

    /* Buttons */
    --sp-button-color:       #212121;
    --sp-button-hover:       #2e2e2e;
    --sp-button-active:      #003e52;
    --sp-button-pressed:     #3a3a3a;
    --sp-button-text-color:  #aaaaaa;

    /* Fonts */
    --sp-font-family:        "Inter, system-ui, sans-serif";
    --sp-font-size:          12px;
    --sp-font-size-large:    32px;
    --sp-font-size-marquee:  14px;
}
```

- [ ] **Step 2: Create the Light template file**

`src/skin_templates/light.css`:

```css
/* Sparkamp Light — Basic Skin Template
 *
 * Edit these 14 values and save this file to
 * ~/.config/sparkamp/skins/<name>.css, then load it from
 * Settings → Appearance → Add skin…
 */
:root {
    /* Colors */
    --sp-background:         #ededed;
    --sp-text-background:    #f6f6f6;
    --sp-text-color:         #222222;
    --sp-highlight:          #1a6fc2;
    --sp-broken-color:       #cc5500;

    /* Buttons */
    --sp-button-color:       #dcdcdc;
    --sp-button-hover:       #cccccc;
    --sp-button-active:      #cce5f7;
    --sp-button-pressed:     #bbbbbb;
    --sp-button-text-color:  #333333;

    /* Fonts */
    --sp-font-family:        "Inter, system-ui, sans-serif";
    --sp-font-size:          12px;
    --sp-font-size-large:    32px;
    --sp-font-size-marquee:  14px;
}
```

- [ ] **Step 3: Create the how-to guide**

`src/skin_templates/skin-guide.md`:

```markdown
# Sparkamp Skin Guide

## What a skin is

A Sparkamp skin is a single `.css` file declaring 14 variables inside a
`:root { }` block. The same file drives Sparkamp's appearance identically
on Linux (GTK4) and macOS.

## Creating your first skin

1. In Settings → Appearance, select Dark or Light.
2. Click **Download skin…** and save it as e.g. `mytheme.css`.
3. Open the file in any text editor.
4. Edit the values inside the `:root { }` block.
5. In Settings → Appearance, click **Add skin…** and pick your file.

The skin applies immediately. To switch, click a different row in the
skin list.

## The 14 variables

### Colors

**`--sp-background`** — `#rgb` or `#rrggbb`
The window chrome behind everything else.
- Main player window frame
- Media Library window frame (outside the listview)
- Settings, ID3 Editor, Dedupe, Artwork, Keyboard Shortcuts, Jump to
  Track, Information, Equalizer window frames

**`--sp-text-background`** — `#rgb` or `#rrggbb`
The "panel" color — darker/contrasting areas that hold text.
- Marquee panel (now-playing title and artist area)
- Time display background
- Playlist scrollable area
- Media Library listview area
- Settings / ID3 Editor input fields and text boxes

**`--sp-text-color`** — `#rgb` or `#rrggbb`
All body text, on both `background` and `text-background` surfaces.
- Marquee title, marquee artist, time digits
- Playlist rows, Media Library cells
- Settings labels, ID3 Editor field text
- Information window body, Jump window
- Keyboard Shortcuts, Dedupe, Artwork, Equalizer text

**`--sp-highlight`** — `#rgb` or `#rrggbb`
Selection, focus, and active-state accent.
- Currently-playing playlist row text
- Currently-selected list/table row background (~18% opacity)
- Active (toggled-on) mode buttons: shuffle, repeat, PL, Info
- Seek and volume bar fills
- Focus ring

**`--sp-broken-color`** — `#rgb` or `#rrggbb`
The warning color for missing/unplayable files.
- Playlist row text for a broken track
- Media Library row text for a broken track
- The `✗` prefix glyph in front of each broken row

### Buttons

**`--sp-button-color`** — resting state
**`--sp-button-hover`** — mouse over
**`--sp-button-active`** — toggled ON (only for toggle buttons: shuffle,
repeat, PL, Info)
**`--sp-button-pressed`** — being clicked right now (mouse held down)
**`--sp-button-text-color`** — icon / label on every button state

Applies to: transport buttons (prev / play / pause / stop / next), mode
toggle buttons, playlist buttons (Add / Remove / Clear), dialog buttons
across all windows.

### Fonts

**`--sp-font-family`** — CSS font-family string
Applies to all text except time digits (which are always monospace).
Example: `"Inter, Helvetica, sans-serif"`. The first installed family wins.

**`--sp-font-size`** — e.g. `12px`
Applies to: playlist rows, ML cells, settings labels, ID3 editor, Jump,
Information, Keyboard Shortcuts, Dedupe, Artwork, Equalizer, all buttons,
volume %, status bar.

**`--sp-font-size-large`** — e.g. `32px`
Time index display only.

**`--sp-font-size-marquee`** — e.g. `14px`
Marquee title in the now-playing panel.

## Auto-derived (not user-facing)

These are computed in code; your skin does not set them directly:

- Selected row background — `highlight` at 18% opacity
- Playing row background — `highlight` at 10% opacity
- Hover row background — `highlight` at 8% opacity
- Seek / volume track background — `text-background`
- Seek / volume fill and thumb — `highlight`
- Muted / dim text (duration column, volume %) — `text-color` at 60% opacity
- Window and panel borders — `background` luminance ±8%
- Time-digit font family — hardcoded monospace

## Tips

- Keep `text-color` and `text-background` at high contrast for readability.
- `highlight` should be visually distinct from both `text-color` and
  `text-background` (it colors text in some places and backgrounds in others).
- Order button state colors perceptually: `color` → `hover` → `pressed`
  so clicks feel responsive. `active` can be a different hue entirely
  (the spec uses an accent-tinted color).

## Limits

This basic template is about colors, fonts, and sizes. It does not
support structural changes (paddings, margins, window sizes, corner
radii) or images. An advanced template with fine-grained overrides and
button image packs is planned for a later release.
```

- [ ] **Step 4: Embed the three files in `src/skin.rs`**

Replace the existing `DARK_CSS_RAW` / `LIGHT_CSS_RAW` / `BUILTIN_SKINS` constants with:

```rust
/// The built-in Dark skin template (also what Download skin… exports for Dark).
pub const DARK_TEMPLATE_CSS: &str = include_str!("skin_templates/dark.css");

/// The built-in Light skin template.
pub const LIGHT_TEMPLATE_CSS: &str = include_str!("skin_templates/light.css");

/// The bundled skin how-to guide (Markdown).
pub const SKIN_GUIDE_MD: &str = include_str!("skin_templates/skin-guide.md");
```

- [ ] **Step 5: Add parse tests for the built-ins**

```rust
#[test]
fn dark_template_parses_cleanly() {
    let v = parse_skin_vars(DARK_TEMPLATE_CSS);
    assert_eq!(v.background.to_hex(), "#1a1a1a");
    assert_eq!(v.highlight.to_hex(), "#00ccff");
}

#[test]
fn light_template_parses_cleanly() {
    let v = parse_skin_vars(LIGHT_TEMPLATE_CSS);
    assert_eq!(v.background.to_hex(), "#ededed");
    assert_eq!(v.highlight.to_hex(), "#1a6fc2");
}

#[test]
fn skin_guide_is_non_empty() {
    assert!(!SKIN_GUIDE_MD.is_empty());
    assert!(SKIN_GUIDE_MD.contains("14 variables"));
}
```

- [ ] **Step 6: Run tests**

```
cargo test --lib skin::tests
```

Expected: pass.

---

### Task 7: load_skin

**Files:**
- Modify: `src/skin.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn load_skin_dark_is_builtin() {
    let s = load_skin("dark").expect("dark exists");
    assert_eq!(s.name, "dark");
    assert!(matches!(s.source, SkinSource::BuiltIn));
    assert_eq!(s.vars.background.to_hex(), "#1a1a1a");
}

#[test]
fn load_skin_light_is_builtin() {
    let s = load_skin("light").expect("light exists");
    assert_eq!(s.name, "light");
    assert!(matches!(s.source, SkinSource::BuiltIn));
}

#[test]
fn load_skin_is_case_insensitive() {
    assert!(load_skin("Dark").is_some());
    assert!(load_skin("LIGHT").is_some());
}

#[test]
fn load_skin_unknown_returns_none() {
    assert!(load_skin("nonexistent_skin_xyz").is_none());
}
```

- [ ] **Step 2: Run tests to verify they fail**

```
cargo test --lib skin::tests::load_skin_
```

Expected: existing `load_skin` returns old `Skin` shape; tests may fail at field access (`.vars`, `.source`).

- [ ] **Step 3: Replace `load_skin`**

Replace the existing `load_skin` function body with:

```rust
pub fn load_skin(name: &str) -> Option<Skin> {
    let lower = name.to_lowercase();

    // User file wins.
    let user_path = user_skins_dir().join(format!("{lower}.css"));
    if user_path.exists() {
        if let Ok(css) = std::fs::read_to_string(&user_path) {
            let vars = parse_skin_vars(&css);
            return Some(Skin {
                name: lower,
                source: SkinSource::UserFile(user_path),
                vars,
            });
        }
    }

    // Built-ins.
    match lower.as_str() {
        "dark" => Some(Skin {
            name: lower,
            source: SkinSource::BuiltIn,
            vars: parse_skin_vars(DARK_TEMPLATE_CSS),
        }),
        "light" => Some(Skin {
            name: lower,
            source: SkinSource::BuiltIn,
            vars: parse_skin_vars(LIGHT_TEMPLATE_CSS),
        }),
        _ => None,
    }
}
```

Delete `load_prepared` and `load_from_path` (unused going forward).

- [ ] **Step 4: Run tests to verify they pass**

```
cargo test --lib skin::tests::load_skin_
```

Expected: pass.

---

### Task 8: list_skins with hidden-list filter

**Files:**
- Modify: `src/skin.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn list_skins_no_user_returns_only_builtins() {
    // This test does not touch the real skins dir — it runs in a temp home.
    let tmp = tempfile::tempdir().unwrap();
    let entries = list_skins_in(tmp.path(), &[]);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].name, "dark");
    assert_eq!(entries[1].name, "light");
}

#[test]
fn list_skins_hidden_filters_user_entries() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("mine.css"),
        ":root { --sp-background: #000000; }").unwrap();
    std::fs::write(tmp.path().join("other.css"),
        ":root { --sp-background: #111111; }").unwrap();

    let all = list_skins_in(tmp.path(), &[]);
    assert_eq!(all.len(), 4); // dark, light, mine, other

    let filtered = list_skins_in(tmp.path(), &["mine".to_string()]);
    let names: Vec<_> = filtered.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["dark", "light", "other"]);
}

#[test]
fn list_skins_hidden_ignores_builtin_names() {
    let tmp = tempfile::tempdir().unwrap();
    let entries = list_skins_in(tmp.path(),
        &["dark".to_string(), "light".to_string()]);
    // Built-ins are never filtered.
    assert_eq!(entries.len(), 2);
}

#[test]
fn list_skins_ignores_non_css_files() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("readme.txt"), "not a skin").unwrap();
    let entries = list_skins_in(tmp.path(), &[]);
    assert_eq!(entries.len(), 2); // just the built-ins
}
```

Also add `tempfile = "3"` to `[dev-dependencies]` in `Cargo.toml` if not already present:

```
cargo add --dev tempfile
```

- [ ] **Step 2: Run tests to verify they fail**

```
cargo test --lib skin::tests::list_skins_
```

Expected: compile errors (`list_skins_in` missing).

- [ ] **Step 3: Implement `list_skins` and testable variant**

```rust
/// Public API: list skins, scanning the real user skins dir.
pub fn list_skins(hidden: &[String]) -> Vec<SkinEntry> {
    list_skins_in(&user_skins_dir(), hidden)
}

/// Test-friendly: list skins, scanning `dir` for `.css` files.
pub fn list_skins_in(dir: &Path, hidden: &[String]) -> Vec<SkinEntry> {
    let mut out = vec![
        SkinEntry::builtin("dark", "Dark"),
        SkinEntry::builtin("light", "Light"),
    ];

    let mut user: Vec<SkinEntry> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map_or(false, |ext| ext == "css"))
            .filter_map(|p| {
                let stem = p.file_stem()?.to_str()?.to_lowercase();
                Some(SkinEntry::user(&stem, p))
            })
            .collect(),
        Err(_) => Vec::new(),
    };

    user.sort_by(|a, b| a.name.cmp(&b.name));
    user.retain(|e| !hidden.iter().any(|h| h.eq_ignore_ascii_case(&e.name)));
    out.extend(user);
    out
}
```

- [ ] **Step 4: Run tests to verify they pass**

```
cargo test --lib skin::tests::list_skins_
```

Expected: pass.

---

### Task 9: add_user_skin

**Files:**
- Modify: `src/skin.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn add_user_skin_copies_to_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("skins");
    std::fs::create_dir_all(&dir).unwrap();

    let src = tmp.path().join("external.css");
    std::fs::write(&src, ":root { --sp-background: #123456; }").unwrap();

    let entry = add_user_skin_to(&src, &dir).unwrap();
    assert_eq!(entry.name, "external");
    assert!(entry.path.as_ref().unwrap().starts_with(&dir));
    assert!(entry.path.as_ref().unwrap().exists());
}

#[test]
fn add_user_skin_uniquifies_collision() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("skins");
    std::fs::create_dir_all(&dir).unwrap();

    // Pre-existing file with the same name.
    std::fs::write(dir.join("mine.css"), ":root { }").unwrap();

    let src = tmp.path().join("mine.css");
    std::fs::write(&src, ":root { --sp-background: #aabbcc; }").unwrap();

    let entry = add_user_skin_to(&src, &dir).unwrap();
    assert_ne!(entry.name, "mine"); // got uniquified
    assert!(entry.name.starts_with("mine-"));
    assert!(entry.path.as_ref().unwrap().exists());
    // Original was not overwritten.
    let original = std::fs::read_to_string(dir.join("mine.css")).unwrap();
    assert_eq!(original, ":root { }");
}

#[test]
fn add_user_skin_rejects_missing_root_block() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("skins");
    std::fs::create_dir_all(&dir).unwrap();

    let src = tmp.path().join("bad.css");
    std::fs::write(&src, "body { color: red; }").unwrap();

    let err = add_user_skin_to(&src, &dir).unwrap_err();
    assert!(matches!(err, SkinError::NoRootBlock));
    // Nothing was copied.
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
}

#[test]
fn add_user_skin_rejects_missing_source() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("skins");
    std::fs::create_dir_all(&dir).unwrap();

    let err = add_user_skin_to(
        &tmp.path().join("does-not-exist.css"), &dir).unwrap_err();
    assert!(matches!(err, SkinError::ReadFailed(_)));
}
```

- [ ] **Step 2: Run tests to verify they fail**

```
cargo test --lib skin::tests::add_user_skin_
```

Expected: compile errors.

- [ ] **Step 3: Implement**

```rust
/// Error from add_user_skin.
#[derive(Debug)]
pub enum SkinError {
    ReadFailed(std::io::Error),
    WriteFailed(std::io::Error),
    NoRootBlock,
}

impl std::fmt::Display for SkinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkinError::ReadFailed(e) => write!(f, "could not read skin file: {e}"),
            SkinError::WriteFailed(e) => write!(f, "could not write skin file: {e}"),
            SkinError::NoRootBlock => write!(f,
                "skin file has no :root block — this is not a valid Sparkamp skin"),
        }
    }
}

impl std::error::Error for SkinError {}

/// Public API: add a skin from `src` into the user skins dir.
pub fn add_user_skin(src: &Path) -> Result<SkinEntry, SkinError> {
    let dir = user_skins_dir();
    let _ = std::fs::create_dir_all(&dir);
    add_user_skin_to(src, &dir)
}

/// Test-friendly: add a skin into an arbitrary directory.
pub fn add_user_skin_to(src: &Path, dir: &Path) -> Result<SkinEntry, SkinError> {
    let css = std::fs::read_to_string(src).map_err(SkinError::ReadFailed)?;
    if extract_root_block(&strip_css_comments(&css)).is_none() {
        return Err(SkinError::NoRootBlock);
    }

    let stem = src.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("skin")
        .to_lowercase();

    let (final_stem, dest) = uniquify(dir, &stem);
    std::fs::copy(src, &dest).map_err(SkinError::WriteFailed)?;
    Ok(SkinEntry::user(&final_stem, dest))
}

fn uniquify(dir: &Path, stem: &str) -> (String, PathBuf) {
    let candidate = dir.join(format!("{stem}.css"));
    if !candidate.exists() {
        return (stem.to_string(), candidate);
    }
    for n in 2..10_000 {
        let s = format!("{stem}-{n}");
        let p = dir.join(format!("{s}.css"));
        if !p.exists() {
            return (s, p);
        }
    }
    // Fallback — extremely unlikely; overwrite with a timestamp.
    let s = format!("{stem}-{}", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0));
    (s.clone(), dir.join(format!("{s}.css")))
}
```

- [ ] **Step 4: Run tests to verify they pass**

```
cargo test --lib skin::tests::add_user_skin_
```

Expected: pass.

---

### Task 10: render_gtk_css — minimal

**Files:**
- Modify: `src/skin.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn render_gtk_css_contains_window_background() {
    let v = SkinVars::dark_defaults();
    let css = render_gtk_css(&v);
    assert!(css.contains("window"));
    assert!(css.contains("background-color: #1a1a1a"));
}

#[test]
fn render_gtk_css_substitutes_text_color() {
    let mut v = SkinVars::dark_defaults();
    v.text_color = Rgb { r: 0xff, g: 0x00, b: 0x00 };
    let css = render_gtk_css(&v);
    assert!(css.contains("color: #ff0000"));
}

#[test]
fn render_gtk_css_substitutes_font_family() {
    let mut v = SkinVars::dark_defaults();
    v.font_family = "Verdana, sans-serif".to_string();
    let css = render_gtk_css(&v);
    assert!(css.contains("font-family: Verdana, sans-serif"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

```
cargo test --lib skin::tests::render_gtk_css_
```

Expected: compile error — `render_gtk_css` missing.

- [ ] **Step 3: Implement `render_gtk_css` (skeleton)**

```rust
/// Render a complete GTK4 stylesheet from the given skin vars.
///
/// The output includes every widget class used by Sparkamp's GTK frontend.
/// Derivations (row backgrounds, dim text, borders) are inlined from
/// the vars at emit time.
pub fn render_gtk_css(v: &SkinVars) -> String {
    use std::fmt::Write;
    let mut css = String::with_capacity(4096);

    // Window chrome
    writeln!(&mut css, "window {{ \
        background-color: {bg}; color: {fg}; \
        font-family: {ff}; font-size: {fs}px; \
    }}",
        bg = v.background.to_hex(),
        fg = v.text_color.to_hex(),
        ff = v.font_family,
        fs = v.font_size,
    ).unwrap();

    css
}
```

- [ ] **Step 4: Run tests to verify they pass**

```
cargo test --lib skin::tests::render_gtk_css_
```

Expected: pass. Task 11 extends `render_gtk_css` to cover all widget classes.

---

### Task 11: render_gtk_css — full coverage

**Files:**
- Modify: `src/skin.rs`

- [ ] **Step 1: Write failing tests for the full surface**

```rust
#[test]
fn render_gtk_css_covers_marquee_panel() {
    let v = SkinVars::dark_defaults();
    let css = render_gtk_css(&v);
    assert!(css.contains(".np-frame"));
    assert!(css.contains(".np-title"));
    assert!(css.contains(".np-artist"));
    assert!(css.contains("font-size: 14px")); // marquee size
}

#[test]
fn render_gtk_css_covers_time_display() {
    let v = SkinVars::dark_defaults();
    let css = render_gtk_css(&v);
    assert!(css.contains(".time-disp"));
    assert!(css.contains("font-size: 32px")); // large size
    assert!(css.contains("font-family: monospace")); // hardcoded
}

#[test]
fn render_gtk_css_covers_transport_buttons_all_states() {
    let v = SkinVars::dark_defaults();
    let css = render_gtk_css(&v);
    assert!(css.contains("button.transport"));
    assert!(css.contains("button.transport:hover"));
    assert!(css.contains("button.transport:active"));
    assert!(css.contains("background-color: #212121")); // button-color
    assert!(css.contains("background-color: #2e2e2e")); // button-hover
    assert!(css.contains("background-color: #3a3a3a")); // button-pressed
}

#[test]
fn render_gtk_css_covers_mode_button_toggle_on() {
    let v = SkinVars::dark_defaults();
    let css = render_gtk_css(&v);
    assert!(css.contains("button.mode-btn"));
    assert!(css.contains("button.mode-btn.mode-btn-active"));
    assert!(css.contains("background-color: #003e52")); // button-active
}

#[test]
fn render_gtk_css_covers_playlist() {
    let v = SkinVars::dark_defaults();
    let css = render_gtk_css(&v);
    assert!(css.contains(".playlist"));
    assert!(css.contains(".playlist row.playing"));
    assert!(css.contains(".playlist row.broken"));
    assert!(css.contains("color: #ff7700")); // broken-color
    assert!(css.contains("rgba(0, 204, 255, 0.1)")); // playing-row bg
    assert!(css.contains("rgba(0, 204, 255, 0.18)")); // selected-row bg
}

#[test]
fn render_gtk_css_covers_seek_and_volume() {
    let v = SkinVars::dark_defaults();
    let css = render_gtk_css(&v);
    assert!(css.contains("scale.seek-scale"));
    assert!(css.contains("scale.vol-scale"));
}

#[test]
fn render_gtk_css_covers_scrolled_window_and_columnview() {
    let v = SkinVars::dark_defaults();
    let css = render_gtk_css(&v);
    assert!(css.contains("columnview") || css.contains("listview"));
    assert!(css.contains(".pl-dur-label"));
}

#[test]
fn render_gtk_css_emits_button_text_color() {
    let mut v = SkinVars::dark_defaults();
    v.button_text_color = Rgb { r: 0x11, g: 0x22, b: 0x33 };
    let css = render_gtk_css(&v);
    assert!(css.contains("color: #112233"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

```
cargo test --lib skin::tests::render_gtk_css_
```

Expected: many failures (minimal impl from Task 10 is incomplete).

- [ ] **Step 3: Replace `render_gtk_css` with full implementation**

Replace the body with the following. The function builds one string with every selector Sparkamp's GTK UI uses. Constants at the top make derivations explicit:

```rust
pub fn render_gtk_css(v: &SkinVars) -> String {
    use std::fmt::Write;
    let mut css = String::with_capacity(8192);

    // Derivations
    let bg     = v.background.to_hex();
    let tbg    = v.text_background.to_hex();
    let text   = v.text_color.to_hex();
    let hl     = v.highlight.to_hex();
    let broken = v.broken_color.to_hex();
    let btn    = v.button_color.to_hex();
    let bhov   = v.button_hover.to_hex();
    let bact   = v.button_active.to_hex();
    let bprs   = v.button_pressed.to_hex();
    let btext  = v.button_text_color.to_hex();
    let ff     = &v.font_family;
    let fs     = v.font_size;
    let fsl    = v.font_size_large;
    let fsm    = v.font_size_marquee;
    let hl_sel = v.highlight.with_opacity(0.18);
    let hl_pla = v.highlight.with_opacity(0.10);
    let hl_hov = v.highlight.with_opacity(0.08);
    let text_dim = v.text_color.with_opacity(0.60);
    // Borders: lighten-on-dark / darken-on-light by 8% luminance.
    let border = derive_border(&v.background).to_hex();

    // Window + default typography
    writeln!(css, "window {{ \
        background-color: {bg}; color: {text}; \
        font-family: {ff}; font-size: {fs}px; \
    }}").unwrap();

    // Secondary / dialog window chrome
    writeln!(css, "dialog, .sparkamp-dialog {{ \
        background-color: {bg}; color: {text}; \
    }}").unwrap();

    // Marquee / Now-Playing frame
    writeln!(css, ".np-frame {{ \
        background-color: {tbg}; border: 1px solid {border}; \
        border-radius: 4px; padding: 4px; \
    }}").unwrap();
    writeln!(css, ".np-title {{ \
        color: {text}; font-size: {fsm}px; font-weight: bold; padding: 2px 0px; \
    }}").unwrap();
    writeln!(css, ".np-artist {{ \
        color: {text_dim}; font-size: {fs}px; padding: 0px 0px 2px 0px; \
    }}").unwrap();

    // Time display (hardcoded monospace)
    writeln!(css, ".time-disp {{ \
        color: {text}; background-color: {tbg}; \
        font-family: monospace; font-size: {fsl}px; \
        padding: 2px 6px; border-radius: 3px; \
    }}").unwrap();

    // Transport buttons
    writeln!(css, "button.transport {{ \
        background-color: {btn}; background-image: none; color: {btext}; \
        border: 1px solid {border}; border-radius: 3px; \
        padding: 4px 8px; box-shadow: none; \
    }}").unwrap();
    writeln!(css, "button.transport:hover {{ \
        background-color: {bhov}; background-image: none; \
    }}").unwrap();
    writeln!(css, "button.transport:active {{ \
        background-color: {bprs}; background-image: none; \
    }}").unwrap();

    // Play button accent (same as transport with an active tint)
    writeln!(css, "button.transport-play {{ \
        background-color: {bact}; background-image: none; \
        color: {btext}; border: 1px solid {hl}; \
    }}").unwrap();

    // Mode toggle buttons (shuffle / repeat / PL / Info)
    writeln!(css, "button.mode-btn {{ \
        background-color: {btn}; background-image: none; color: {btext}; \
        border: 1px solid {border}; border-radius: 3px; padding: 2px 6px; \
    }}").unwrap();
    writeln!(css, "button.mode-btn:hover {{ \
        background-color: {bhov}; background-image: none; \
    }}").unwrap();
    writeln!(css, "button.mode-btn:active {{ \
        background-color: {bprs}; background-image: none; \
    }}").unwrap();
    writeln!(css, "button.mode-btn.mode-btn-active {{ \
        background-color: {bact}; background-image: none; color: {btext}; \
    }}").unwrap();

    // Seek bar
    writeln!(css, "scale.seek-scale trough {{ \
        background-color: {tbg}; background-image: none; \
        min-height: 4px; \
    }}").unwrap();
    writeln!(css, "scale.seek-scale highlight {{ \
        background-color: {hl}; background-image: none; \
    }}").unwrap();
    writeln!(css, "scale.seek-scale slider {{ \
        background-color: {hl}; background-image: none; \
        border-radius: 50%; min-width: 10px; min-height: 10px; \
    }}").unwrap();

    // Volume slider
    writeln!(css, "scale.vol-scale trough {{ \
        background-color: {tbg}; background-image: none; \
    }}").unwrap();
    writeln!(css, "scale.vol-scale highlight {{ \
        background-color: {hl}; background-image: none; \
    }}").unwrap();
    writeln!(css, "scale.vol-scale slider {{ \
        background-color: {hl}; background-image: none; \
    }}").unwrap();
    writeln!(css, ".vol-label {{ \
        color: {text_dim}; font-size: {fs}px; font-family: monospace; min-width: 28px; \
    }}").unwrap();

    // Mini visualizer
    writeln!(css, ".mini-viz {{ \
        background-color: {tbg}; border: 1px solid {border}; \
    }}").unwrap();

    // Playlist + Media Library list/columnview
    writeln!(css, ".playlist, columnview, listview {{ \
        background-color: {tbg}; color: {text}; font-size: {fs}px; \
    }}").unwrap();
    writeln!(css, ".playlist row, columnview row, listview row {{ \
        color: {text}; \
    }}").unwrap();
    writeln!(css, ".playlist row:hover, columnview row:hover, listview row:hover {{ \
        background: {hl_hov}; \
    }}").unwrap();
    writeln!(css, ".playlist row:selected, columnview row:selected, listview row:selected {{ \
        background: {hl_sel}; color: {text}; \
    }}").unwrap();
    writeln!(css, ".playlist row.playing {{ \
        background-color: {hl_pla}; color: {hl}; \
    }}").unwrap();
    writeln!(css, ".playlist row.playing label, .playlist row.playing cell {{ \
        color: {hl}; \
    }}").unwrap();
    writeln!(css, ".playlist row.broken, columnview row.broken {{ \
        color: {broken}; \
    }}").unwrap();
    writeln!(css, ".playlist row.broken label, .playlist row.broken cell, \
                   columnview row.broken label, columnview row.broken cell {{ \
        color: {broken}; \
    }}").unwrap();
    writeln!(css, ".pl-dur-label {{ color: {text_dim}; font-family: monospace; }}").unwrap();
    writeln!(css, ".pl-count-label {{ color: {text}; font-size: {fs}px; }}").unwrap();

    // Playlist buttons
    writeln!(css, "button.pl-btn {{ \
        background-color: {btn}; background-image: none; color: {btext}; \
        border: 1px solid {border}; border-radius: 3px; padding: 2px 8px; \
    }}").unwrap();
    writeln!(css, "button.pl-btn:hover {{ \
        background-color: {bhov}; background-image: none; \
    }}").unwrap();
    writeln!(css, "button.pl-btn:active {{ \
        background-color: {bprs}; background-image: none; \
    }}").unwrap();
    writeln!(css, "button.pl-btn.destructive {{ color: {broken}; }}").unwrap();

    // Generic buttons (settings, dialogs, ID3, etc.)
    writeln!(css, "button {{ \
        background-color: {btn}; background-image: none; color: {btext}; \
        border: 1px solid {border}; border-radius: 3px; padding: 4px 10px; \
    }}").unwrap();
    writeln!(css, "button:hover {{ background-color: {bhov}; background-image: none; }}").unwrap();
    writeln!(css, "button:active {{ background-color: {bprs}; background-image: none; }}").unwrap();

    // Status bar + info text
    writeln!(css, ".status-label {{ color: {text_dim}; font-size: {fs}px; }}").unwrap();
    writeln!(css, ".info-text {{ \
        color: {text}; background-color: {tbg}; font-family: monospace; font-size: {fs}px; \
        padding: 6px; border-radius: 3px; \
    }}").unwrap();

    // Form inputs sitting on text-background
    writeln!(css, "entry, textview {{ \
        background-color: {tbg}; color: {text}; caret-color: {hl}; \
        border: 1px solid {border}; \
    }}").unwrap();

    css
}

/// Derive a subtle border color from a background: ±8% luminance.
fn derive_border(bg: &Rgb) -> Rgb {
    let delta: i16 = if bg.luminance() < 0.5 { 20 } else { -20 };
    let clamp = |c: u8| -> u8 {
        let x = c as i16 + delta;
        x.clamp(0, 255) as u8
    };
    Rgb { r: clamp(bg.r), g: clamp(bg.g), b: clamp(bg.b) }
}
```

- [ ] **Step 4: Run tests**

```
cargo test --lib skin::tests
```

Expected: all skin tests pass. No warnings.

---

## Phase 2 — GTK Integration

### Task 12: Config field swap

**Files:**
- Modify: `src/config.rs`

- [ ] **Step 1: Write failing test**

Add to the existing tests in `src/config.rs`:

```rust
#[test]
fn appearance_config_default_uses_active_skin_dark() {
    let cfg = AppearanceConfig::default();
    assert_eq!(cfg.active_skin, "dark");
    assert!(cfg.hidden_skins.is_empty());
}

#[test]
fn appearance_config_deserialize_from_new_format() {
    let toml_str = r#"
active_skin = "mytheme"
hidden_skins = ["ugly-skin"]
"#;
    let cfg: AppearanceConfig = toml::from_str(toml_str).expect("parse");
    assert_eq!(cfg.active_skin, "mytheme");
    assert_eq!(cfg.hidden_skins, vec!["ugly-skin"]);
}

#[test]
fn appearance_config_deserialize_from_empty_defaults_active_skin() {
    let cfg: AppearanceConfig = toml::from_str("").expect("parse");
    assert_eq!(cfg.active_skin, "dark");
}
```

- [ ] **Step 2: Run to see them fail**

```
cargo test --lib config::tests::appearance_config_
```

Expected: compile errors (`active_skin` missing, or conflicts with removed fields).

- [ ] **Step 3: Replace `AppearanceConfig`**

In `src/config.rs`:

1. **Delete** the `ThemeChoice` enum (lines ~285–293).
2. **Delete** the `AccentColorChoice` enum and its `impl Default` / `impl AccentColorChoice` block (lines ~295–346).
3. Replace the `AppearanceConfig` struct body with:

```rust
/// Visual-appearance preferences that live under `[appearance]` in the TOML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppearanceConfig {
    /// Name of the active skin — either a built-in (`"dark"` or
    /// `"light"`) or the filename stem of a `.css` file in
    /// `~/.config/sparkamp/skins/`.
    #[serde(default = "default_active_skin")]
    pub active_skin: String,

    /// Skin names the user has removed from the Appearance picker.
    /// Built-ins (`"dark"`, `"light"`) cannot appear here.
    #[serde(default)]
    pub hidden_skins: Vec<String>,
}

fn default_active_skin() -> String {
    "dark".to_string()
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        AppearanceConfig {
            active_skin: default_active_skin(),
            hidden_skins: Vec::new(),
        }
    }
}
```

4. **Delete** the old tests that exercise `theme`, `accent_color`, `custom_skin` (lines ~954–990 in the current file). Keep the new tests you added in Step 1.

- [ ] **Step 4: Run tests**

```
cargo test --lib config
```

Expected: pass.

- [ ] **Step 5: Check for callers of removed symbols**

```
cargo build 2>&1 | head -80
```

Expect errors in `frontends/gtk/window.rs` (uses `ThemeChoice`, `AccentColorChoice`, `theme`, `accent_color`, `custom_skin`) — these are fixed in Task 13/14. Do not fix anything else during this task; the errors are expected until Phase 2 completes.

---

### Task 13: Wire render_gtk_css into GTK startup; delete static CSS

**Files:**
- Modify: `frontends/gtk/window.rs` (lines ~746–747, ~1150–1200)
- Delete: `frontends/gtk/style.css`, `frontends/gtk/style_dark.css`, `frontends/gtk/style_light.css`

- [ ] **Step 1: Delete the static CSS files**

```
rm frontends/gtk/style.css frontends/gtk/style_dark.css frontends/gtk/style_light.css
```

- [ ] **Step 2: Replace the imports in `frontends/gtk/window.rs`**

Find line ~747:
```rust
use crate::skin::{prepare_css, DARK_CSS_RAW, LIGHT_CSS_RAW};
```
Replace with:
```rust
use crate::skin::{self, render_gtk_css, SkinVars};
```

- [ ] **Step 3: Replace the startup CSS-load block**

Find the block starting "// ── CSS theme ─────" near line ~1150 and ending at line ~1200 where `provider_for_settings` / `dark_css_for_settings` / `light_css_for_settings` are cloned. Replace the entire block with:

```rust
    // ── CSS theme ─────────────────────────────────────────────────────────────
    // Load the active skin from config. Fall back to Dark if the named
    // skin cannot be resolved.
    let initial_vars = skin::load_skin(&config.appearance.active_skin)
        .map(|s| s.vars)
        .unwrap_or_else(SkinVars::dark_defaults);
    let initial_css = render_gtk_css(&initial_vars);

    let provider = Rc::new(gtk4::CssProvider::new());
    provider.load_from_data(&initial_css);
    gtk4::style_context_add_provider_for_display(
        &gdk::Display::default().expect("No display"),
        &*provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    // Use the dark Adwaita variant for built-in widgets whenever the
    // skin's window background is dark.
    let initial_dark = initial_vars.background.luminance() < 0.5;
    if let Some(gtk_settings) = gtk4::Settings::default() {
        gtk_settings.set_gtk_application_prefer_dark_theme(initial_dark);
    }

    // Cloned Rc references used by the Appearance tab handlers.
    let provider_for_settings = provider.clone();
```

- [ ] **Step 4: Delete the old `resolve_accent_hex` helper and any now-unused helpers**

Search for `fn resolve_accent_hex` in `frontends/gtk/window.rs` and delete its definition. Remove remaining references to `dark_css_rc`, `light_css_rc`, `accent_hex_initial`, `accent_hex_current`, `dark_mode` (the `Rc<Cell<bool>>`), and `dark_css_for_settings` / `light_css_for_settings` bindings. Use the compiler as a guide.

- [ ] **Step 5: Verify the rest of window.rs still has many errors (Appearance tab)**

```
cargo build 2>&1 | grep "^error" | head -30
```

Expect errors localized to the Appearance-tab area of `open_settings_window`. These are fixed in Task 14.

---

### Task 14: Rewrite GTK Appearance tab

**Files:**
- Modify: `frontends/gtk/window.rs` (lines ~5734–6006 — the Appearance tab block in `open_settings_window`)

- [ ] **Step 1: Locate the Appearance tab block**

In `open_settings_window`, find the block labeled `// ── Tab 0: Appearance ─────` near line ~5734 and ending where `notebook.append_page(&grid, Some(&tab_lbl))` fires for the Appearance tab near line ~6006.

- [ ] **Step 2: Replace with the new list-based UI**

Replace the entire Appearance-tab block body with:

```rust
    // ── Tab 0: Appearance ─────────────────────────────────────────────────
    {
        use gtk4::{Box as GtkBox, Button, Label, ListBox, ListBoxRow, Orientation,
                   PolicyType, ScrolledWindow, SelectionMode, FileDialog, FileFilter};
        use glib::clone;

        let root = GtkBox::new(Orientation::Vertical, 10);
        root.set_margin_top(16);
        root.set_margin_bottom(16);
        root.set_margin_start(16);
        root.set_margin_end(16);

        // Header
        let header = Label::new(Some("Skin"));
        header.set_halign(Align::Start);
        header.add_css_class("heading");
        root.append(&header);

        // Scrollable list of skins
        let listbox = ListBox::new();
        listbox.set_selection_mode(SelectionMode::Single);
        listbox.add_css_class("rich-list");

        let scrolled = ScrolledWindow::new();
        scrolled.set_policy(PolicyType::Never, PolicyType::Automatic);
        scrolled.set_min_content_height(200);
        scrolled.set_child(Some(&listbox));
        root.append(&scrolled);

        // Populate rows
        let rebuild_list = {
            let listbox = listbox.clone();
            let state_rc = state.clone();
            Rc::new(move || {
                // Clear.
                while let Some(row) = listbox.first_child() {
                    listbox.remove(&row);
                }
                let hidden = state_rc.borrow().config.appearance.hidden_skins.clone();
                let entries = crate::skin::list_skins(&hidden);
                let active = state_rc.borrow().config.appearance.active_skin.clone();
                let mut active_row: Option<ListBoxRow> = None;

                for entry in entries {
                    let row = ListBoxRow::new();
                    let hbox = GtkBox::new(Orientation::Horizontal, 8);
                    hbox.set_margin_top(4);
                    hbox.set_margin_bottom(4);
                    hbox.set_margin_start(8);
                    hbox.set_margin_end(8);

                    let name_lbl = Label::new(Some(&entry.display_name));
                    name_lbl.set_halign(Align::Start);
                    name_lbl.set_hexpand(true);
                    hbox.append(&name_lbl);

                    if entry.is_builtin {
                        let tag = Label::new(Some("(built-in)"));
                        tag.add_css_class("dim-label");
                        hbox.append(&tag);
                    }

                    if entry.name == active {
                        let mark = Label::new(Some("● Active"));
                        mark.add_css_class("dim-label");
                        hbox.append(&mark);
                    }

                    row.set_child(Some(&hbox));
                    // Store the skin name on the row via widget name for retrieval.
                    row.set_widget_name(&entry.name);
                    listbox.append(&row);
                    if entry.name == active {
                        active_row = Some(row);
                    }
                }
                if let Some(r) = active_row {
                    listbox.select_row(Some(&r));
                }
            })
        };
        rebuild_list();

        // Selecting a row applies the skin live.
        {
            let state_rc = state.clone();
            let provider = provider_for_settings.clone();
            listbox.connect_row_selected(move |_, row| {
                let Some(row) = row else { return };
                let name = row.widget_name().to_string();
                if name.is_empty() { return; }
                let Some(skin) = crate::skin::load_skin(&name) else { return };
                let css = crate::skin::render_gtk_css(&skin.vars);
                provider.load_from_data(&css);
                if let Some(gtk_settings) = gtk4::Settings::default() {
                    gtk_settings.set_gtk_application_prefer_dark_theme(
                        skin.vars.background.luminance() < 0.5);
                }
                state_rc.borrow_mut().config.appearance.active_skin = name;
            });
        }

        // Row of action buttons
        let btn_row = GtkBox::new(Orientation::Horizontal, 8);
        let btn_add = Button::with_label("Add skin…");
        let btn_remove = Button::with_label("Remove");
        let btn_download = Button::with_label("Download skin…");
        btn_row.append(&btn_add);
        btn_row.append(&btn_remove);
        btn_row.append(&btn_download);
        root.append(&btn_row);

        // Wire Add
        {
            let state_rc = state.clone();
            let rebuild = rebuild_list.clone();
            let listbox = listbox.clone();
            let win = window.clone();
            btn_add.connect_clicked(move |_| {
                let dialog = FileDialog::new();
                dialog.set_title("Add Sparkamp skin");
                let filter = FileFilter::new();
                filter.add_suffix("css");
                filter.set_name(Some("Sparkamp skin (*.css)"));
                let filters = gio::ListStore::new::<FileFilter>();
                filters.append(&filter);
                dialog.set_filters(Some(&filters));

                let state_rc = state_rc.clone();
                let rebuild = rebuild.clone();
                let listbox = listbox.clone();
                dialog.open(Some(&win), gio::Cancellable::NONE, move |res| {
                    let Ok(file) = res else { return };
                    let Some(path) = file.path() else { return };
                    match crate::skin::add_user_skin(&path) {
                        Ok(entry) => {
                            state_rc.borrow_mut().config.appearance.active_skin =
                                entry.name.clone();
                            // Un-hide if it was hidden
                            state_rc.borrow_mut().config.appearance.hidden_skins
                                .retain(|n| !n.eq_ignore_ascii_case(&entry.name));
                            rebuild();
                            // Select the newly-added row.
                            if let Some(row) = find_row_by_name(&listbox, &entry.name) {
                                listbox.select_row(Some(&row));
                            }
                        }
                        Err(e) => {
                            // Surface the error via a simple AlertDialog.
                            let msg = format!("Could not add skin: {e}");
                            show_error_alert(&msg);
                        }
                    }
                });
            });
        }

        // Wire Remove (disabled for built-ins)
        {
            let state_rc = state.clone();
            let rebuild = rebuild_list.clone();
            let listbox = listbox.clone();
            btn_remove.connect_clicked(move |_| {
                let Some(row) = listbox.selected_row() else { return };
                let name = row.widget_name().to_string();
                if name == "dark" || name == "light" || name.is_empty() {
                    return;
                }
                {
                    let mut s = state_rc.borrow_mut();
                    if !s.config.appearance.hidden_skins.iter().any(|h| h.eq_ignore_ascii_case(&name)) {
                        s.config.appearance.hidden_skins.push(name.clone());
                    }
                    if s.config.appearance.active_skin == name {
                        s.config.appearance.active_skin = "dark".to_string();
                    }
                }
                rebuild();
            });
        }

        // Update Remove-disabled state reactively on selection changes.
        {
            let btn_remove = btn_remove.clone();
            listbox.connect_row_selected(move |_, row| {
                let name = row.map(|r| r.widget_name().to_string()).unwrap_or_default();
                let is_builtin = name == "dark" || name == "light" || name.is_empty();
                btn_remove.set_sensitive(!is_builtin);
            });
        }

        // Wire Download (Export template CSS…)
        {
            let state_rc = state.clone();
            let listbox = listbox.clone();
            let win = window.clone();
            btn_download.connect_clicked(move |_| {
                let Some(row) = listbox.selected_row() else { return };
                let name = row.widget_name().to_string();
                let Some(skin) = crate::skin::load_skin(&name) else { return };

                let dialog = FileDialog::new();
                dialog.set_title("Save Sparkamp skin");
                dialog.set_initial_name(Some(&format!("{name}.css")));
                let _ = state_rc; // suppress unused-warning

                let skin_copy = skin.clone();
                dialog.save(Some(&win), gio::Cancellable::NONE, move |res| {
                    let Ok(file) = res else { return };
                    let Some(path) = file.path() else { return };
                    let css = match &skin_copy.source {
                        crate::skin::SkinSource::BuiltIn => match skin_copy.name.as_str() {
                            "dark" => crate::skin::DARK_TEMPLATE_CSS.to_string(),
                            "light" => crate::skin::LIGHT_TEMPLATE_CSS.to_string(),
                            _ => crate::skin::DARK_TEMPLATE_CSS.to_string(),
                        },
                        crate::skin::SkinSource::UserFile(p) => {
                            std::fs::read_to_string(p).unwrap_or_default()
                        }
                    };
                    let _ = std::fs::write(&path, css);
                });
            });
        }

        // Separator
        let sep = gtk4::Separator::new(Orientation::Horizontal);
        sep.set_margin_top(8);
        sep.set_margin_bottom(8);
        root.append(&sep);

        // Documentation header + button
        let doc_header = Label::new(Some("Documentation"));
        doc_header.set_halign(Align::Start);
        doc_header.add_css_class("heading");
        root.append(&doc_header);

        let btn_guide = Button::with_label("Export how-to guide…");
        root.append(&btn_guide);
        {
            let win = window.clone();
            btn_guide.connect_clicked(move |_| {
                let dialog = FileDialog::new();
                dialog.set_title("Save Sparkamp skin guide");
                dialog.set_initial_name(Some("sparkamp-skin-guide.md"));
                dialog.save(Some(&win), gio::Cancellable::NONE, move |res| {
                    let Ok(file) = res else { return };
                    let Some(path) = file.path() else { return };
                    let _ = std::fs::write(&path, crate::skin::SKIN_GUIDE_MD);
                });
            });
        }

        let tab_lbl = Label::new(Some("Appearance"));
        notebook.append_page(&root, Some(&tab_lbl));
    }
```

- [ ] **Step 3: Add two small helpers to `frontends/gtk/window.rs`**

Place near the other free helpers in the file (grep for existing `fn show_error_alert` first — if it doesn't exist, add this):

```rust
fn find_row_by_name(listbox: &gtk4::ListBox, name: &str) -> Option<gtk4::ListBoxRow> {
    let mut child = listbox.first_child();
    while let Some(c) = child {
        if let Ok(row) = c.clone().downcast::<gtk4::ListBoxRow>() {
            if row.widget_name().as_str() == name {
                return Some(row);
            }
        }
        child = c.next_sibling();
    }
    None
}

fn show_error_alert(msg: &str) {
    let alert = gtk4::AlertDialog::builder()
        .message("Sparkamp")
        .detail(msg)
        .modal(true)
        .build();
    alert.show(gtk4::Window::NONE);
}
```

- [ ] **Step 4: Build**

```
cargo build 2>&1 | tail -40
```

Expected: clean build. If the compiler flags unused imports (e.g. old `DropDown`, `Entry` usages that were Accent/Theme-related), delete them.

- [ ] **Step 5: Run the test suite**

```
cargo test --lib
```

Expected: all pass.

---

### Task 15: GTK manual verification

**Files:** none (runtime verification only)

- [ ] **Step 1: Run the GTK UI**

```
cargo run --bin sparkamp -- --ui
```

- [ ] **Step 2: Verify player window applies Dark by default**

Expected: dark background, cyan highlight on the time display, Winamp-style look matching the previous Dark skin (visually similar — exact pixels may differ).

- [ ] **Step 3: Verify Appearance tab**

Open Settings → Appearance. Expected:
- A scrollable list showing `Dark (built-in) ● Active` and `Light (built-in)`.
- `Remove` button is disabled.
- Three buttons: Add skin…, Remove, Download skin…
- Below a separator, a `Documentation` heading and an `Export how-to guide…` button.

- [ ] **Step 4: Click Light in the list**

Expected: the player window re-themes to the Light palette immediately (off-white background, blue highlight, dark text). No restart required.

- [ ] **Step 5: Click Dark again**

Expected: re-themes back to Dark.

- [ ] **Step 6: Test Download skin…**

Select Dark, click Download skin…, save as `~/dark-exported.css`. Verify the file contains the 14 `--sp-*` variables.

- [ ] **Step 7: Test Add skin…**

Edit `~/dark-exported.css`: change `--sp-highlight` to `#ff00ff`. Click Add skin…, pick the file. Expected: a new row `Dark-exported` (or similar titlecased name) appears, is selected, and the UI re-themes with a magenta highlight.

- [ ] **Step 8: Test Remove on the user skin**

Select the new row, click Remove. Expected: the row disappears; the UI re-themes to Dark. The file still exists on disk at `~/.config/sparkamp/skins/`.

- [ ] **Step 9: Test Export how-to guide…**

Click Export how-to guide…, save as `~/sparkamp-guide.md`. Expected: the Markdown file contains the full skin guide content.

- [ ] **Step 10: Restart and verify persistence**

Quit Sparkamp, reopen. Expected: the previously active skin is still selected.

**If any step fails, file the defect in the plan's Issues section and continue — do not fix destructively.**

---

### Task 16: GTK cargo warnings cleanup

**Files:**
- Modify: `frontends/gtk/window.rs`

- [ ] **Step 1: Run a clean build**

```
cargo build 2>&1 | grep -E "warning:|^warning$" | head -40
```

- [ ] **Step 2: Fix every warning the previous tasks introduced**

Common categories:
- Unused imports: delete.
- Dead code: if it's genuinely unreachable (e.g. old `ThemeChoice`-related helper), delete. If it's a helper the Appearance tab no longer uses, delete.

- [ ] **Step 3: Verify zero warnings**

```
cargo build 2>&1 | grep -c "^warning"
```

Expected: `0`.

---

## Phase 3 — macOS Theme refactor

> **Important:** Phase 3 and Phase 4 tasks modify Swift files that cannot be built from the Linux dev machine. The executor writes the changes exactly as specified; correctness is verified later by the user on a macOS build machine.

### Task 17: Refactor SkinTheme in Theme.swift

**Files:**
- Modify: `frontends/SparkampMac/Sources/Theme.swift`

- [ ] **Step 1: Replace the `SkinTheme` struct**

Locate the `struct SkinTheme` definition (around line 49). Replace the entire type plus the `defaultDark` / `defaultLight` static factories with:

```swift
// MARK: - SkinVars (mirrors Rust SkinVars)

struct SkinVars {
    var background:       Color
    var textBackground:   Color
    var textColor:        Color
    var highlight:        Color
    var brokenColor:      Color

    var buttonColor:      Color
    var buttonHover:      Color
    var buttonActive:     Color
    var buttonPressed:    Color
    var buttonTextColor:  Color

    var fontFamily:       String
    var fontSize:         CGFloat
    var fontSizeLarge:    CGFloat
    var fontSizeMarquee:  CGFloat
}

extension SkinVars {
    // ── Dark defaults (mirrors Rust SkinVars::dark_defaults) ──────────────
    static let dark = SkinVars(
        background:       Color(hex: "#1a1a1a")!,
        textBackground:   Color(hex: "#0c0c0c")!,
        textColor:        Color(hex: "#cccccc")!,
        highlight:        Color(hex: "#00ccff")!,
        brokenColor:      Color(hex: "#ff7700")!,

        buttonColor:      Color(hex: "#212121")!,
        buttonHover:      Color(hex: "#2e2e2e")!,
        buttonActive:     Color(hex: "#003e52")!,
        buttonPressed:    Color(hex: "#3a3a3a")!,
        buttonTextColor:  Color(hex: "#aaaaaa")!,

        fontFamily:       "Inter, system-ui, sans-serif",
        fontSize:         12,
        fontSizeLarge:    32,
        fontSizeMarquee:  14
    )

    // ── Light defaults (mirrors Rust SkinVars::light_defaults) ────────────
    static let light = SkinVars(
        background:       Color(hex: "#ededed")!,
        textBackground:   Color(hex: "#f6f6f6")!,
        textColor:        Color(hex: "#222222")!,
        highlight:        Color(hex: "#1a6fc2")!,
        brokenColor:      Color(hex: "#cc5500")!,

        buttonColor:      Color(hex: "#dcdcdc")!,
        buttonHover:      Color(hex: "#cccccc")!,
        buttonActive:     Color(hex: "#cce5f7")!,
        buttonPressed:    Color(hex: "#bbbbbb")!,
        buttonTextColor:  Color(hex: "#333333")!,

        fontFamily:       "Inter, system-ui, sans-serif",
        fontSize:         12,
        fontSizeLarge:    32,
        fontSizeMarquee:  14
    )
}

// MARK: - Derived values

extension SkinVars {
    /// 18%-opacity highlight for selected rows.
    var selectedRowBg: Color { highlight.opacity(0.18) }
    /// 10%-opacity highlight for the currently-playing row.
    var playingRowBg:  Color { highlight.opacity(0.10) }
    /// 8%-opacity highlight for row hover.
    var hoverRowBg:    Color { highlight.opacity(0.08) }
    /// 60%-opacity text color for duration column, volume %, etc.
    var dimTextColor:  Color { textColor.opacity(0.60) }
    /// Auto-derived border color — ±8% luminance vs background.
    var borderColor:   Color {
        let nsColor = NSColor(background).usingColorSpace(.sRGB) ?? .gray
        var r: CGFloat = 0, g: CGFloat = 0, b: CGFloat = 0
        nsColor.getRed(&r, green: &g, blue: &b, alpha: nil)
        let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b
        let delta: CGFloat = lum < 0.5 ? 0.08 : -0.08
        return Color(red: max(0, min(1, r + delta)),
                     green: max(0, min(1, g + delta)),
                     blue: max(0, min(1, b + delta)))
    }
    /// Whether this skin's background is dark enough to use Apple's dark scheme.
    var prefersDark: Bool {
        let nsColor = NSColor(background).usingColorSpace(.sRGB) ?? .gray
        var r: CGFloat = 0, g: CGFloat = 0, b: CGFloat = 0
        nsColor.getRed(&r, green: &g, blue: &b, alpha: nil)
        return (0.2126 * r + 0.7152 * g + 0.0722 * b) < 0.5
    }
}

// MARK: - Font helpers

extension SkinVars {
    /// Body font (family + standard size) for inheritable defaults.
    var bodyFont: Font {
        .custom(fontFamily, size: fontSize)
    }
    /// Marquee title font (family + marquee size, bold).
    var marqueeFont: Font {
        .custom(fontFamily, size: fontSizeMarquee).weight(.bold)
    }
    /// Large display font for the time index (always monospaced).
    var largeMonospaceFont: Font {
        .system(size: fontSizeLarge, weight: .regular, design: .monospaced)
    }
    /// Standard monospaced font for duration column / volume %.
    var smallMonospaceFont: Font {
        .system(size: fontSize, design: .monospaced)
    }
}
```

- [ ] **Step 2: Remove `ButtonImageSet`**

Delete the `struct ButtonImageSet` and the `buttonImages:` field usage. Button images are deferred to the advanced template.

- [ ] **Step 3: Compile check (deferred)**

The file will not compile until `CSSParser` and `ThemeManager` are updated. That happens in Tasks 18 and 19.

---

### Task 18: Simplify CSSParser for the 14-variable schema

**Files:**
- Modify: `frontends/SparkampMac/Sources/Theme.swift` (the `enum CSSParser` block)

- [ ] **Step 1: Replace the parser body**

Replace the entire `enum CSSParser { ... }` block with:

```swift
// MARK: - CSSParser

/// Parses a Sparkamp skin CSS file and fills a `SkinVars`. Missing or
/// malformed variables fall back to Dark defaults per-field. Parsing
/// never fails; a completely empty input produces `.dark`.
enum CSSParser {
    /// Parse a CSS string and return the resolved vars.
    static func parse(css: String) -> SkinVars {
        var vars = SkinVars.dark
        let stripped = stripComments(css)
        guard let root = extractRootBlock(stripped) else { return vars }
        for statement in root.components(separatedBy: ";") {
            let trimmed = statement.trimmingCharacters(in: .whitespacesAndNewlines)
            guard trimmed.hasPrefix("--sp-") else { continue }
            let parts = trimmed.split(separator: ":", maxSplits: 1)
                .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            guard parts.count == 2 else { continue }
            apply(key: parts[0], raw: parts[1], to: &vars)
        }
        return vars
    }

    /// Load a skin from a URL.
    static func load(url: URL) -> SkinVars? {
        guard let css = try? String(contentsOf: url, encoding: .utf8) else { return nil }
        return parse(css: css)
    }

    // MARK: Private

    private static func stripComments(_ css: String) -> String {
        var out = ""
        var i = css.startIndex
        while i < css.endIndex {
            let next = css.index(after: i)
            if css[i] == "/", next < css.endIndex, css[next] == "*" {
                if let r = css.range(of: "*/", range: css.index(i, offsetBy: 2)..<css.endIndex) {
                    i = r.upperBound
                    continue
                } else {
                    break
                }
            }
            out.append(css[i])
            i = next
        }
        return out
    }

    private static func extractRootBlock(_ css: String) -> String? {
        guard let rootRange = css.range(of: ":root") else { return nil }
        let afterRoot = css[rootRange.upperBound...]
        guard let openRel = afterRoot.firstIndex(of: "{") else { return nil }
        let afterOpen = afterRoot[afterRoot.index(after: openRel)...]
        guard let closeRel = afterOpen.firstIndex(of: "}") else { return nil }
        return String(afterOpen[..<closeRel])
    }

    private static func apply(key: String, raw: String, to vars: inout SkinVars) {
        switch key {
        case "--sp-background":        if let c = Color(hex: raw) { vars.background        = c }
        case "--sp-text-background":   if let c = Color(hex: raw) { vars.textBackground    = c }
        case "--sp-text-color":        if let c = Color(hex: raw) { vars.textColor         = c }
        case "--sp-highlight":         if let c = Color(hex: raw) { vars.highlight         = c }
        case "--sp-broken-color":      if let c = Color(hex: raw) { vars.brokenColor       = c }
        case "--sp-button-color":      if let c = Color(hex: raw) { vars.buttonColor       = c }
        case "--sp-button-hover":      if let c = Color(hex: raw) { vars.buttonHover       = c }
        case "--sp-button-active":     if let c = Color(hex: raw) { vars.buttonActive      = c }
        case "--sp-button-pressed":    if let c = Color(hex: raw) { vars.buttonPressed     = c }
        case "--sp-button-text-color": if let c = Color(hex: raw) { vars.buttonTextColor   = c }
        case "--sp-font-family":       vars.fontFamily = stripQuotes(raw)
        case "--sp-font-size":         if let n = parsePx(raw) { vars.fontSize         = n }
        case "--sp-font-size-large":   if let n = parsePx(raw) { vars.fontSizeLarge    = n }
        case "--sp-font-size-marquee": if let n = parsePx(raw) { vars.fontSizeMarquee  = n }
        default: break
        }
    }

    private static func stripQuotes(_ s: String) -> String {
        let t = s.trimmingCharacters(in: .whitespacesAndNewlines)
        if t.count >= 2,
           (t.first == "\"" && t.last == "\"") || (t.first == "'" && t.last == "'") {
            return String(t.dropFirst().dropLast())
        }
        return t
    }

    private static func parsePx(_ s: String) -> CGFloat? {
        let t = s.trimmingCharacters(in: .whitespacesAndNewlines)
        let num = t.hasSuffix("px") ? String(t.dropLast(2)) : t
        return Double(num.trimmingCharacters(in: .whitespacesAndNewlines)).map { CGFloat($0) }
    }
}
```

---

### Task 19: Rewrite ThemeManager API

**Files:**
- Modify: `frontends/SparkampMac/Sources/Theme.swift` (the `final class ThemeManager` block)

- [ ] **Step 1: Replace ThemeManager**

Replace the entire `final class ThemeManager` body with:

```swift
// MARK: - ThemeManager

@MainActor
final class ThemeManager: ObservableObject {
    @Published private(set) var currentVars: SkinVars
    @Published private(set) var activeSkin: String   // "dark" | "light" | user stem

    // Storage
    private static let activeSkinKey = "sparkamp.activeSkin"
    private static let hiddenSkinsKey = "sparkamp.hiddenSkins"

    init() {
        let saved = UserDefaults.standard.string(forKey: Self.activeSkinKey) ?? "dark"
        self.activeSkin = saved
        self.currentVars = Self.load(skinName: saved) ?? .dark
    }

    // MARK: Skin registry

    struct SkinEntry: Identifiable, Equatable {
        var name: String             // "dark", "light", or user stem
        var displayName: String
        var isBuiltin: Bool
        var path: URL?
        var id: String { name }
    }

    /// Returns the skin list: built-ins + user-dir `.css` files,
    /// minus hidden entries (built-ins are never filterable).
    func listSkins() -> [SkinEntry] {
        var out = [
            SkinEntry(name: "dark",  displayName: "Dark",  isBuiltin: true,  path: nil),
            SkinEntry(name: "light", displayName: "Light", isBuiltin: true,  path: nil),
        ]
        let dir = Self.userSkinsDir()
        let hidden = Set((UserDefaults.standard.stringArray(forKey: Self.hiddenSkinsKey) ?? [])
            .map { $0.lowercased() })
        if let urls = try? FileManager.default.contentsOfDirectory(
            at: dir, includingPropertiesForKeys: nil) {
            let sorted = urls
                .filter { $0.pathExtension.lowercased() == "css" }
                .sorted { $0.lastPathComponent < $1.lastPathComponent }
            for url in sorted {
                let stem = url.deletingPathExtension().lastPathComponent.lowercased()
                if hidden.contains(stem) { continue }
                out.append(SkinEntry(
                    name: stem,
                    displayName: titlecase(stem),
                    isBuiltin: false,
                    path: url))
            }
        }
        return out
    }

    // MARK: Active skin

    func setActiveSkin(_ name: String) {
        let vars = Self.load(skinName: name) ?? .dark
        self.activeSkin = name
        self.currentVars = vars
        UserDefaults.standard.set(name, forKey: Self.activeSkinKey)
    }

    // MARK: Add / Hide

    @discardableResult
    func addUserSkin(from source: URL) -> SkinEntry? {
        let dir = Self.userSkinsDir()
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        guard let css = try? String(contentsOf: source, encoding: .utf8),
              CSSParser.parse(css: css).background != nil || true  // validate by attempting parse
        else { return nil }
        // Validate the file has a :root block.
        guard css.range(of: ":root") != nil else { return nil }

        let stem = source.deletingPathExtension().lastPathComponent.lowercased()
        let (finalStem, dest) = uniquify(dir: dir, stem: stem)
        do {
            try FileManager.default.copyItem(at: source, to: dest)
        } catch {
            return nil
        }

        // Un-hide if it was hidden.
        var hidden = UserDefaults.standard.stringArray(forKey: Self.hiddenSkinsKey) ?? []
        hidden.removeAll { $0.caseInsensitiveCompare(finalStem) == .orderedSame }
        UserDefaults.standard.set(hidden, forKey: Self.hiddenSkinsKey)

        return SkinEntry(name: finalStem, displayName: titlecase(finalStem),
                         isBuiltin: false, path: dest)
    }

    func hideSkin(_ name: String) {
        guard name != "dark", name != "light" else { return }
        var hidden = UserDefaults.standard.stringArray(forKey: Self.hiddenSkinsKey) ?? []
        if !hidden.contains(where: { $0.caseInsensitiveCompare(name) == .orderedSame }) {
            hidden.append(name)
            UserDefaults.standard.set(hidden, forKey: Self.hiddenSkinsKey)
        }
        if activeSkin == name {
            setActiveSkin("dark")
        }
    }

    // MARK: Export

    func exportSkin(_ name: String, to destination: URL) {
        let css: String
        switch name.lowercased() {
        case "dark":  css = Self.darkTemplateCSS
        case "light": css = Self.lightTemplateCSS
        default:
            let src = Self.userSkinsDir().appendingPathComponent("\(name).css")
            css = (try? String(contentsOf: src, encoding: .utf8)) ?? ""
        }
        try? css.write(to: destination, atomically: true, encoding: .utf8)
    }

    func exportGuide(to destination: URL) {
        try? Self.skinGuideMD.write(to: destination, atomically: true, encoding: .utf8)
    }

    // MARK: Internals

    private static func userSkinsDir() -> URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".config/sparkamp/skins")
    }

    private static func load(skinName: String) -> SkinVars? {
        switch skinName.lowercased() {
        case "dark":  return CSSParser.parse(css: darkTemplateCSS)
        case "light": return CSSParser.parse(css: lightTemplateCSS)
        default:
            let path = userSkinsDir().appendingPathComponent("\(skinName).css")
            guard FileManager.default.fileExists(atPath: path.path) else { return nil }
            return CSSParser.load(url: path)
        }
    }

    private func uniquify(dir: URL, stem: String) -> (String, URL) {
        let candidate = dir.appendingPathComponent("\(stem).css")
        if !FileManager.default.fileExists(atPath: candidate.path) {
            return (stem, candidate)
        }
        for n in 2..<10_000 {
            let s = "\(stem)-\(n)"
            let p = dir.appendingPathComponent("\(s).css")
            if !FileManager.default.fileExists(atPath: p.path) {
                return (s, p)
            }
        }
        let s = "\(stem)-\(Int(Date().timeIntervalSince1970))"
        return (s, dir.appendingPathComponent("\(s).css"))
    }

    // MARK: Embedded template sources

    static let darkTemplateCSS: String = """
    /* Sparkamp Dark — Basic Skin Template */
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
    """

    static let lightTemplateCSS: String = """
    /* Sparkamp Light — Basic Skin Template */
    :root {
        --sp-background:         #ededed;
        --sp-text-background:    #f6f6f6;
        --sp-text-color:         #222222;
        --sp-highlight:          #1a6fc2;
        --sp-broken-color:       #cc5500;
        --sp-button-color:       #dcdcdc;
        --sp-button-hover:       #cccccc;
        --sp-button-active:      #cce5f7;
        --sp-button-pressed:     #bbbbbb;
        --sp-button-text-color:  #333333;
        --sp-font-family:        "Inter, system-ui, sans-serif";
        --sp-font-size:          12px;
        --sp-font-size-large:    32px;
        --sp-font-size-marquee:  14px;
    }
    """

    static let skinGuideMD: String = """
    See the canonical guide at
    src/skin_templates/skin-guide.md — copied here during build.
    """
    // NOTE: The guide is kept as a single source of truth on the Rust side.
    // During macOS app bundling, copy src/skin_templates/skin-guide.md into
    // the .app bundle and load it at runtime. For this basic template,
    // embedding as a literal is acceptable; replace the `skinGuideMD`
    // placeholder above with the real content by pasting it verbatim
    // (it matches the content of src/skin_templates/skin-guide.md).
}

// Title-case helper (mirrors Rust titlecase()).
private func titlecase(_ stem: String) -> String {
    stem.split(whereSeparator: { $0 == "-" || $0 == "_" })
        .map { $0.prefix(1).uppercased() + $0.dropFirst() }
        .joined(separator: " ")
}
```

- [ ] **Step 2: Paste the guide content**

Open `src/skin_templates/skin-guide.md` and paste its full content into `Theme.swift`'s `skinGuideMD` static constant (replacing the placeholder text). Use Swift's triple-quote raw string. Commit this as manual duplication — the advanced template will add a bundling step to dedupe.

---

### Task 20: Rewrite AppearancePane

**Files:**
- Modify: `frontends/SparkampMac/Sources/SettingsWindow.swift` (the `private struct AppearancePane` block around line 125)

- [ ] **Step 1: Replace AppearancePane**

Replace the entire `private struct AppearancePane` block with:

```swift
// MARK: - Appearance pane

private struct AppearancePane: View {
    @EnvironmentObject var themeManager: ThemeManager

    @State private var selection: String? = nil
    @State private var entries: [ThemeManager.SkinEntry] = []
    @State private var errorMessage: String? = nil

    var body: some View {
        Form {
            Section("Skin") {
                List(entries, selection: $selection) { entry in
                    HStack {
                        Text(entry.displayName)
                        if entry.isBuiltin {
                            Text("(built-in)").foregroundStyle(.secondary)
                        }
                        Spacer()
                        if entry.name == themeManager.activeSkin {
                            Image(systemName: "checkmark.circle.fill")
                                .foregroundStyle(.blue)
                        }
                    }
                    .tag(entry.name)
                }
                .frame(minHeight: 180)
                .onChange(of: selection) { _, new in
                    if let new {
                        themeManager.setActiveSkin(new)
                    }
                }

                HStack {
                    Button("Add skin…")       { addSkin() }
                    Button("Remove")          { removeSelected() }
                        .disabled(isBuiltinSelected)
                    Button("Download skin…")  { downloadSelected() }
                        .disabled(selection == nil)
                }
            }

            Section("Documentation") {
                Button("Export how-to guide…") { exportGuide() }
            }
        }
        .formStyle(.grouped)
        .alert("Could not add skin",
               isPresented: Binding(
                   get: { errorMessage != nil },
                   set: { if !$0 { errorMessage = nil } })) {
            Button("OK") { errorMessage = nil }
        } message: {
            Text(errorMessage ?? "")
        }
        .onAppear {
            entries = themeManager.listSkins()
            selection = themeManager.activeSkin
        }
    }

    // MARK: Actions

    private var isBuiltinSelected: Bool {
        guard let s = selection else { return true }
        return s == "dark" || s == "light"
    }

    private func addSkin() {
        let panel = NSOpenPanel()
        panel.title = "Add Sparkamp skin"
        panel.allowedContentTypes = [.init(filenameExtension: "css")!]
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.begin { response in
            guard response == .OK, let url = panel.url else { return }
            Task { @MainActor in
                if let entry = themeManager.addUserSkin(from: url) {
                    entries = themeManager.listSkins()
                    themeManager.setActiveSkin(entry.name)
                    selection = entry.name
                } else {
                    errorMessage = "The file is not a valid Sparkamp skin (missing :root block or unreadable)."
                }
            }
        }
    }

    private func removeSelected() {
        guard let s = selection, !isBuiltinSelected else { return }
        themeManager.hideSkin(s)
        entries = themeManager.listSkins()
        selection = themeManager.activeSkin
    }

    private func downloadSelected() {
        guard let s = selection else { return }
        let panel = NSSavePanel()
        panel.title = "Save Sparkamp skin"
        panel.nameFieldStringValue = "\(s).css"
        panel.allowedContentTypes = [.init(filenameExtension: "css")!]
        panel.begin { response in
            guard response == .OK, let url = panel.url else { return }
            Task { @MainActor in
                themeManager.exportSkin(s, to: url)
            }
        }
    }

    private func exportGuide() {
        let panel = NSSavePanel()
        panel.title = "Save Sparkamp skin guide"
        panel.nameFieldStringValue = "sparkamp-skin-guide.md"
        panel.allowedContentTypes = [.init(filenameExtension: "md")!]
        panel.begin { response in
            guard response == .OK, let url = panel.url else { return }
            Task { @MainActor in
                themeManager.exportGuide(to: url)
            }
        }
    }
}
```

---

### Task 21: macOS root font application

**Files:**
- Modify: `frontends/SparkampMac/Sources/SparkampMacApp.swift` (if present; otherwise the app-root view)

- [ ] **Step 1: Set a default font at the view root**

Find the top-level `WindowGroup` / root view in `SparkampMacApp.swift`. Wrap the root content in:

```swift
.font(themeManager.currentVars.bodyFont)
.foregroundStyle(themeManager.currentVars.textColor)
.tint(themeManager.currentVars.highlight)
.preferredColorScheme(themeManager.currentVars.prefersDark ? .dark : .light)
```

so that every SwiftUI view inherits the default body font and text color. Individual views that need a larger or smaller size override locally.

If a root-level font modifier already exists (e.g. set by the old `ThemeManager.preferredColorScheme`), replace it with the above snippet.

---

## Phase 4 — macOS font propagation sweep

Each task in this phase touches one or two files. The pattern is identical:

1. For every `.font(.system(size: N))` call, pick the appropriate theme font:
    - `theme.bodyFont` for standard body text
    - `theme.marqueeFont` for the marquee title only
    - `theme.largeMonospaceFont` for the time display only
    - `theme.smallMonospaceFont` for duration / volume monospace bits
2. For every hardcoded `Color(...)` not already driven by a theme property, replace with the appropriate `theme.*` value.
3. Remove any `@EnvironmentObject private var themeManager: ThemeManager` duplicates if they exist; leave exactly one per view.

> **Local alias:** Inside each view, add `let theme = themeManager.currentVars` at the top of `body` so the replacements read cleanly.

### Task 22: MarqueeView.swift + PlayerWindow.swift + ContentView.swift

**Files:**
- Modify: `frontends/SparkampMac/Sources/MarqueeView.swift`
- Modify: `frontends/SparkampMac/Sources/PlayerWindow.swift`
- Modify: `frontends/SparkampMac/Sources/ContentView.swift`

- [ ] **Step 1: Sweep `MarqueeView.swift`**

Top of body:
```swift
let theme = themeManager.currentVars
```
Replace every `.font(.system(size: N))` where the view renders the marquee title with `.font(theme.marqueeFont)`. Replace artist subtitles with `.font(theme.bodyFont).foregroundStyle(theme.dimTextColor)`. Container backgrounds that used `.background(Color.black)` or similar become `.background(theme.textBackground)`.

- [ ] **Step 2: Sweep `PlayerWindow.swift`**

Same pattern. The time display widget becomes:
```swift
Text(timeString)
    .font(theme.largeMonospaceFont)
    .foregroundStyle(theme.textColor)
    .padding(.horizontal, 6)
    .background(theme.textBackground)
    .clipShape(RoundedRectangle(cornerRadius: 3))
```
Volume % uses `.font(theme.smallMonospaceFont).foregroundStyle(theme.dimTextColor)`.

- [ ] **Step 3: Sweep `ContentView.swift`**

Wherever it hosts the marquee or time widgets, ensure they're inside `.environmentObject(themeManager)` and don't re-set a different font. Remove any `.font(.system(size: N))` that would shadow the root body font unless it needs to be explicitly different.

- [ ] **Step 4: Visual self-check**

Read your changes. Every `.font(.system(size: ...))` should now either use a theme font or be intentionally a different size with a comment explaining why.

---

### Task 23: PlaylistView.swift + MediaLibraryWindow.swift

**Files:**
- Modify: `frontends/SparkampMac/Sources/PlaylistView.swift`
- Modify: `frontends/SparkampMac/Sources/MediaLibraryWindow.swift`

- [ ] **Step 1: Sweep `PlaylistView.swift`**

Top of body:
```swift
let theme = themeManager.currentVars
```
- Row text → `.font(theme.bodyFont).foregroundStyle(theme.textColor)`.
- Playing row → add `.background(theme.playingRowBg)` and text color `theme.highlight`.
- Broken row → text color `theme.brokenColor`; prefix with `"✗ "` in the display string.
- Duration column → `.font(theme.smallMonospaceFont).foregroundStyle(theme.dimTextColor)`.
- Scrollable area background → `.background(theme.textBackground)`.
- Selected row (if manually styled) → `.background(theme.selectedRowBg)`.

- [ ] **Step 2: Sweep `MediaLibraryWindow.swift`**

Same patterns. Additionally:
- Header row text → `.font(theme.bodyFont).foregroundStyle(theme.textColor)`.
- Path bar → `.font(theme.bodyFont)`.
- Column headers → `.font(theme.bodyFont).bold()`.
- Broken ML rows → `theme.brokenColor` + `"✗ "` prefix.

---

### Task 24: Id3EditorWindow.swift + JumpToTrackView.swift + KeyboardShortcutsView.swift

**Files:**
- Modify: `frontends/SparkampMac/Sources/Id3EditorWindow.swift`
- Modify: `frontends/SparkampMac/Sources/JumpToTrackView.swift`
- Modify: `frontends/SparkampMac/Sources/KeyboardShortcutsView.swift`

- [ ] **Step 1: Sweep each file**

For every label, replace hardcoded fonts with `theme.bodyFont`. For every `TextField` / `TextEditor`, apply:

```swift
.background(theme.textBackground)
.foregroundStyle(theme.textColor)
.tint(theme.highlight)
```

Shortcut-key chips in `KeyboardShortcutsView.swift` use `theme.smallMonospaceFont`.

---

### Task 25: DeduplicatorWindow.swift + ArtworkWindow.swift + EqualizerWindow.swift

**Files:**
- Modify: `frontends/SparkampMac/Sources/DeduplicatorWindow.swift`
- Modify: `frontends/SparkampMac/Sources/ArtworkWindow.swift`
- Modify: `frontends/SparkampMac/Sources/EqualizerWindow.swift`

- [ ] **Step 1: Sweep each file**

- All labels → `theme.bodyFont`.
- List/table backgrounds → `theme.textBackground`.
- EQ band labels → `theme.smallMonospaceFont` (they're fixed-width tick values).
- EQ slider thumbs / fills should use the default `.tint(theme.highlight)` from the root modifier — verify by searching for any hardcoded `.tint(.blue)` overrides and removing them.

---

### Task 26: SettingsWindow.swift (non-AppearancePane sweeps)

**Files:**
- Modify: `frontends/SparkampMac/Sources/SettingsWindow.swift`

- [ ] **Step 1: Sweep every pane other than AppearancePane**

Settings pane has multiple tabs (About, Playback, Visualizer, MediaLibrary). For each, replace hardcoded `.font(...)` with `theme.bodyFont` or a deliberate size. Replace hardcoded colors with theme values. The About pane's large app title stays at its explicit size — wrap it with the theme font family:

```swift
Text("Sparkamp")
    .font(.custom(theme.fontFamily, size: 28).weight(.bold))
    .foregroundStyle(theme.textColor)
```

- [ ] **Step 2: Ensure `@EnvironmentObject var themeManager: ThemeManager` is present on every sub-pane**

If any sub-pane is missing it, add it.

---

### Task 27: FullscreenVisualizerWindow.swift + VisualizerView.swift

**Files:**
- Modify: `frontends/SparkampMac/Sources/FullscreenVisualizerWindow.swift`
- Modify: `frontends/SparkampMac/Sources/VisualizerView.swift`

- [ ] **Step 1: Sweep each file**

Visualizer graphics are plugin-driven and don't change. Only text overlays (e.g. a track title label on the fullscreen visualizer) need updating. For each:

- Text overlay → `.font(theme.marqueeFont).foregroundStyle(theme.textColor)` with a semi-transparent `theme.textBackground` backdrop.
- HUD labels → `theme.bodyFont`.

If a file contains no text overlay, leave it untouched and mark the task complete with a comment note.

---

## Phase 5 — Verification

### Task 28: End-to-end verification

**Files:** none (verification only)

- [ ] **Step 1: Full Rust build and test**

```
cargo build 2>&1 | tail -20
cargo test --lib 2>&1 | tail -20
```

Expected: clean build, zero warnings, all tests pass (192 baseline + new skin tests — count should be higher).

- [ ] **Step 2: GTK manual QA**

Re-run the manual steps from Task 15 to confirm nothing regressed during Phase 2 cleanup.

- [ ] **Step 3: macOS build (user-executed)**

Hand back to the user for macOS Xcode build and manual QA.

Expected user-side checks:
- App launches, default Dark skin applied.
- All windows (Main, Playlist, ML, Settings, ID3, Dedupe, Artwork, Jump, Information, Keyboard Shortcuts, Equalizer, Fullscreen Visualizer) show themed colors and fonts.
- Time digits are monospaced regardless of `--sp-font-family` choice.
- Broken tracks show the ✗ prefix and `--sp-broken-color` color.
- Appearance pane: skin list, Add / Remove / Download / Export-guide all function.

- [ ] **Step 4: Report results**

Compile a short report for the user:
- `cargo build` / `cargo test` summary
- GTK smoke test outcome (pass/fail per Task 15 step)
- Files changed (git diff --stat)
- Known deferred items (e.g. guide-MD duplication between Rust and Swift)

Wait for user green-light before any `git commit`.

---

## Self-Review — Spec Coverage

- **Spec §2 — The 14 variables:** covered by Tasks 2 (struct), 3 (parse), 4 (fallbacks), 10–11 (render).
- **Spec §3 — File format:** Tasks 3 and 4 (parser is permissive; `:root` is the contract).
- **Spec §4 — Built-in templates:** Task 6 (embedded files).
- **Spec §5 — User skin directory:** Tasks 8 (list), 9 (add).
- **Spec §6 — Config changes:** Task 12.
- **Spec §7 — Core architecture:** Tasks 1–11 cover types, parser, list, add, render, embedded assets. The removed APIs (`load_prepared`, `load_from_path`, accent injection) are deleted during Tasks 7 and 13.
- **Spec §8 — GTK frontend changes:** Tasks 13 (startup + delete CSS), 14 (Appearance tab), 16 (warnings).
- **Spec §9 — macOS frontend changes:** Tasks 17–20 (Theme.swift and AppearancePane), Task 21 (root font), Tasks 22–27 (font propagation).
- **Spec §10 — Appearance pane UX:** Task 14 (GTK) + Task 20 (macOS).
- **Spec §11 — How-to document:** Task 6 (file content).
- **Spec §12 — Testing strategy:** Tasks 1–11 include unit tests; Task 15 is GTK manual QA; Task 28 is final verification.
- **Spec §13 — Out of scope:** honored throughout (no button images, no structural controls, no zip packaging).
- **Spec §14 — Files touched:** matches file list above.

No gaps found.
