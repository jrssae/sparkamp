# GTK HIG Improvements Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land ten GNOME HIG improvements in the GTK frontend — accessibility, text scaling, packaging metadata, standard shortcuts, notifications, toasts, and empty states — plus one data-loss fix in the TUI, without eroding Sparkamp's Winamp character.

**Architecture:** Core changes are confined to `src/skin.rs` (CSS unit conversion) and `src/config.rs` (one new field). Everything else is GTK frontend work in `frontends/gtk/window/`, plus packaging metadata and a contained TUI keymap fix. libadwaita enters the tree for exactly two widgets — `ToastOverlay` and `StatusPage` — and nothing else.

**Tech Stack:** Rust 2024, GTK4 (gtk4-rs 0.9), libadwaita 0.7, GStreamer, ratatui/crossterm (TUI), Flatpak with an offline vendored Cargo tree.

**Spec:** `docs/superpowers/specs/2026-08-24-gtk-hig-improvements-design.md`

## Global Constraints

Every task's requirements implicitly include this section.

- **Build and test inside the distrobox**, never on the host — the host Fedora image lacks `gstreamer-video-1.0` dev packages:
  `distrobox enter dev-box -- sh -c 'cargo build && cargo test'`
- **Zero warnings, zero test failures** before any task is considered done (`CLAUDE.md`).
- **Scope is GTK/Linux plus one TUI task.** Do not modify `frontends/SparkampMac/` or `frontends/macos/`. Parity gaps are logged in `docs/mac-pass-checklist.md` by Task 12.
- **libadwaita is limited to `ToastOverlay` (Task 8) and `StatusPage` (Task 9).** Introducing any other Adw widget requires a fresh decision from the user — do not substitute `AdwPreferencesPage`, `AdwAboutWindow`, or `AdwBanner` on your own initiative.
- **New config fields carry `#[serde(default)]`** so older config files still load.
- **Product name in user-visible text is "Sparkamp"** — capital S, lowercase a.
- **No version bump.** Do not touch the version in `Cargo.toml`, `README.md`, or the metainfo `<release>` list. Release is a separate, user-confirmed step.
- **Use `gtk_safe()`** on any metadata or error string that reaches a GTK widget — it strips NUL bytes that would otherwise panic.
- **Comments explain why, not what** (`CLAUDE.md`), in plain English.
- The GUI is the **default** invocation: `sparkamp`. There is no `--ui` flag; `--tui` is the only mode flag.

---

## File Structure

**Core (`src/`)**
- `skin.rs` — gains `px_to_pt()`; every font-size CSS emit site switches to `pt`; the two `defaults()` functions re-baseline.
- `skin_templates/dark.css`, `light.css` — re-baselined font values (these are what "Download skin…" exports).
- `skin_templates/skin-guide.md` — a section on how font sizes scale.
- `config.rs` — `PlaybackConfig::notify_track_change`.

**GTK frontend (`frontends/gtk/`)**
- `mod.rs` — `adw::init()` in `connect_startup`.
- `window/util.rs` — `show_toast()` and `empty_state()` helpers, alongside the existing alert helpers.
- `window/player.rs` — shortcut aliases, `shortcut_sections()`, notification subscriber, toast overlay on the main and playlist windows, accessible names.
- `window/keys.rs` — Ctrl+F arm.
- `window/settings.rs` — notification checkbox, tab mnemonics.
- `window/playlist_window.rs` — menu-bar mnemonics, playlist empty state.
- `window/files.rs`, `album_gallery.rs`, `playlists.rs`, `devices_page.rs`, `disc_data.rs` — empty states, accessible row/cell semantics.

**TUI (`frontends/tui/`)**
- `keys.rs` — `/` and Ctrl+F search, F1 help, Remove All in the ops popup.
- `ui/overlays.rs` — ops popup gains Remove All; help text updated.

**Packaging**
- `packaging/dev.sparkamp.Sparkamp.desktop`, `.metainfo.xml` — trademark removal, metainfo completeness.
- `docs/screenshots/` — README already committed; PNGs supplied by the user.

---

## Task 1: libadwaita dependency and initialisation

Gates Tasks 8 and 9. Nothing user-visible ships here — this task only proves Adw links, initialises, and still builds offline.

**Files:**
- Modify: `Cargo.toml` (the `[target.'cfg(target_os = "linux")'.dependencies]` block)
- Modify: `frontends/gtk/mod.rs`
- Modify: `Cargo.lock`, `vendor/`, `packaging/cargo-sources.json` (generated)

**Interfaces:**
- Consumes: nothing.
- Produces: `adw` is in scope for the GTK frontend and initialised before any window is built. Tasks 8 and 9 rely on this.

- [ ] **Step 1: Add the dependency**

In `Cargo.toml`, inside the existing `[target.'cfg(target_os = "linux")'.dependencies]` block (which already holds `gtk4` and `zbus`), add:

```toml
# libadwaita — used for exactly two widgets: AdwToastOverlay (transient
# error feedback) and AdwStatusPage (empty states). Adw's stylesheet loses
# to the skin CSS, which loads at STYLE_PROVIDER_PRIORITY_APPLICATION, so
# skinned surfaces are unaffected. Deliberately NOT used for preferences,
# about, or header bars — those would fight the skin system.
libadwaita = { version = "0.7", features = ["v1_5"] }
```

- [ ] **Step 2: Initialise Adw at application startup**

In `frontends/gtk/mod.rs`, inside `run()`, after the `Application::builder()...build()` call and before `app.connect_open(...)`, add:

```rust
    // libadwaita must be initialised after GTK and before any Adw widget is
    // constructed. `connect_startup` is the first signal GApplication emits
    // after GTK init, which makes it the only correct place for this.
    app.connect_startup(|_| {
        adw::init().expect("libadwaita failed to initialise");
    });
```

- [ ] **Step 3: Build to verify the dependency resolves and links**

Run: `distrobox enter dev-box -- sh -c 'cargo build 2>&1 | tail -20'`
Expected: compiles with zero warnings. If `libadwaita 0.7` reports a gtk4 version mismatch, check which gtk4 minor it pairs with and pin accordingly — do not upgrade gtk4 itself.

- [ ] **Step 4: Run the full suite to confirm nothing regressed**

Run: `distrobox enter dev-box -- sh -c 'cargo test 2>&1 | tail -20'`
Expected: all tests pass, zero warnings.

- [ ] **Step 5: Regenerate the vendored tree**

The Flatpak builds offline from `vendor/` with `.cargo/config.toml` redirecting crates.io to it. Adding a dependency invalidates both the vendor tree and the generated sources file.

```bash
distrobox enter dev-box -- sh -c 'cargo vendor --versioned-dirs vendor'
```

- [ ] **Step 6: Regenerate cargo-sources.json**

Per `packaging/README.md`:

```bash
cd packaging
curl -O https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/master/cargo/flatpak-cargo-generator.py
python3 flatpak-cargo-generator.py ../Cargo.lock -o cargo-sources.json
rm flatpak-cargo-generator.py
cd ..
```

If `aiohttp` is missing, `pip install aiohttp` first. Verify the file grew and is valid JSON:

```bash
python3 -c "import json;d=json.load(open('packaging/cargo-sources.json'));print(len(d),'sources')"
```

Expected: a source count larger than before (the previous file was 168K).

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock frontends/gtk/mod.rs vendor packaging/cargo-sources.json
git commit -m "build: add libadwaita for toasts and empty states

Two widgets need it — AdwToastOverlay and AdwStatusPage — and nothing
else does. Adw's stylesheet loses to the skin CSS, which loads at
STYLE_PROVIDER_PRIORITY_APPLICATION, so skinned surfaces are unaffected;
the drift to watch for is on widgets the skin does not cover.

Initialised from connect_startup, the first signal emitted after GTK
init and the only point where an Adw widget is guaranteed constructible.

The vendored tree and cargo-sources.json are regenerated because the
Flatpak builds offline from them."
```

---

## Task 2: Font sizes scale with the desktop (item 7)

GTK does not scale `px`, so GNOME's large-text setting currently has no effect on any Sparkamp text. This task switches every font-size emit site to `pt` — which GTK converts through `gtk-xft-dpi`, exactly what the text-scaling factor multiplies — and re-baselines the built-in defaults to native 11pt.

**Files:**
- Modify: `src/skin.rs` (helper, ~28 emit sites, both `defaults()` functions, two existing tests)
- Modify: `src/skin_templates/dark.css:27-29`, `src/skin_templates/light.css:24-26`
- Modify: `src/skin_templates/skin-guide.md`
- Test: `mod tests` inside `src/skin.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `fn px_to_pt(px: f32) -> String` (private to `skin.rs`). The public 14-variable skin format is unchanged — `parse_px` still reads `12px`.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/skin.rs`:

```rust
    /// px_to_pt applies the CSS 96dpi reference (1pt = 1/72in, 1px = 1/96in)
    /// and trims a trailing ".0" so the common case reads `9pt`, not `9.0pt`.
    #[test]
    fn px_to_pt_converts_at_the_css_reference() {
        assert_eq!(px_to_pt(12.0), "9pt");
        assert_eq!(px_to_pt(40.0), "30pt");
        assert_eq!(px_to_pt(15.0), "11.25pt");
        assert_eq!(px_to_pt(18.0), "13.5pt");
    }

    /// No font size may survive as `px`: GTK does not scale px, so any that
    /// slipped through would silently ignore the desktop's text scaling.
    #[test]
    fn rendered_css_has_no_px_font_sizes() {
        let css = render_gtk_css(&SkinVars::dark_defaults());
        assert!(
            !css.contains("font-size: ") || !css.contains("px;"),
            "no font-size rule may use px"
        );
        for line in css.lines() {
            if let Some(idx) = line.find("font-size:") {
                let rest = &line[idx..];
                assert!(
                    !rest.starts_with("font-size:") || !rest[..rest.find(';').unwrap_or(rest.len())].contains("px"),
                    "px font size survived: {line}"
                );
            }
        }
    }

    /// The built-in skins land on GNOME's native 11pt. These four values are
    /// mirrored in dark.css and light.css, which is what Download skin…
    /// exports; a change to one that misses the others must fail here.
    #[test]
    fn builtin_defaults_are_baselined_to_native_11pt() {
        for vars in [SkinVars::dark_defaults(), SkinVars::light_defaults()] {
            assert_eq!(vars.font_size, 15.0);
            assert_eq!(vars.font_size_marquee, 18.0);
            assert_eq!(vars.font_size_large, 40.0);
        }
        assert!(DARK_TEMPLATE_CSS.contains("--sp-font-size:          15px;"));
        assert!(DARK_TEMPLATE_CSS.contains("--sp-font-size-large:    40px;"));
        assert!(DARK_TEMPLATE_CSS.contains("--sp-font-size-marquee:  18px;"));
        assert!(LIGHT_TEMPLATE_CSS.contains("--sp-font-size:          15px;"));
        assert!(LIGHT_TEMPLATE_CSS.contains("--sp-font-size-large:    40px;"));
        assert!(LIGHT_TEMPLATE_CSS.contains("--sp-font-size-marquee:  18px;"));
    }

    /// A custom skin's declared sizes are honoured exactly — the re-baseline
    /// moves the built-ins only. A user file wins over the built-in in
    /// load_skin, so custom skins never shift underneath their author.
    #[test]
    fn custom_skin_sizes_are_not_rebaselined() {
        let css_in = ":root { --sp-font-size: 12px; --sp-font-size-large: 32px; \
                      --sp-font-size-marquee: 14px; }";
        let vars = parse_skin_vars(css_in);
        assert_eq!(vars.font_size, 12.0);
        let out = render_gtk_css(&vars);
        assert!(out.contains("font-size: 9pt"));
        assert!(out.contains("font-size: 24pt"));
        assert!(out.contains("font-size: 10.5pt"));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `distrobox enter dev-box -- sh -c 'cargo test --lib skin:: 2>&1 | tail -25'`
Expected: FAIL — `px_to_pt` is not defined, and the default assertions do not match.

- [ ] **Step 3: Add the conversion helper**

In `src/skin.rs`, next to `parse_px` (around line 342):

```rust
/// Render a px font size as CSS `pt`.
///
/// GTK does not scale `px`, so a px font size ignores the desktop's text
/// scaling factor entirely — large-text accessibility mode has no effect on
/// it. `pt` is converted through `gtk-xft-dpi`, which is exactly what that
/// factor multiplies, so the same number both renders at the intended size
/// and grows when the user asks for larger text.
///
/// The factor is the CSS reference resolution of 96dpi: 1pt is 1/72in and
/// 1px is 1/96in, so 1px is 0.75pt.
fn px_to_pt(px: f32) -> String {
    let pt = px * 0.75;
    if pt.fract().abs() < f32::EPSILON {
        format!("{}pt", pt as i32)
    } else {
        format!("{pt}pt")
    }
}
```

- [ ] **Step 4: Convert the emit sites**

In `render_gtk_css`, replace the numeric font bindings (around lines 559-562):

```rust
    let ff     = &v.font_family;
    // Font sizes are pre-rendered as `pt` strings: GTK scales pt with the
    // desktop text-scaling factor but leaves px alone. `fs_px` stays numeric
    // for the two badge variants that derive a smaller size from it.
    let fs_px  = v.font_size;
    let fs     = px_to_pt(fs_px);
    let fsl    = px_to_pt(v.font_size_large);
    let fsm    = px_to_pt(v.font_size_marquee);
    let fs_sm  = px_to_pt(fs_px - 2.0);
    let fs_badge = px_to_pt(16.0);
```

Then, throughout the function, change every `font-size: {fs}px` to `font-size: {fs}`, every `font-size: {fsl}px` to `font-size: {fsl}`, and every `font-size: {fsm}px` to `font-size: {fsm}`. The sites are at lines 573, 587, 590, 608, 631, 704, 730, 761, 823, 853, 866, 870, 888, 930, 934, 938, 942, 946, 950, 953, 956, 1057, 1060, 1067, 1075.

Three sites need individual handling:

```rust
    // line ~658 — stop-after badge glyph, previously a hardcoded 16px
    writeln!(css, "label.stop-after-badge {{ \
        color: {text}; font-size: {fs_badge}; margin: 0; padding: 0; \
    }}").unwrap();

    // line ~874 — small device badge, previously `{}px` with `fs - 2.0`
    writeln!(css, ".device-badge-sm {{ font-size: {fs_sm}; padding: 0px 6px; }}").unwrap();

    // line ~880 — disc source pill, previously `{}px` with `fs - 2.0`
    writeln!(css, ".disc-source-pill {{ \
        color: {text_dim}; background-color: {tbg}; border: 1px solid {border}; \
        border-radius: 999px; padding: 1px 8px; font-size: {fs_sm}; \
    }}").unwrap();
```

Leave the `font-size: 0.85em` at line ~1044 alone — `em` already scales, and it is a relative adjustment inside a popover rather than an absolute size.

- [ ] **Step 5: Re-baseline the built-in defaults**

In `SkinVars::dark_defaults()` (line ~172) and `SkinVars::light_defaults()` (line ~194), change both blocks identically:

```rust
            font_family:       "Inter, system-ui, sans-serif".to_string(),
            font_size:         15.0,
            font_size_large:   40.0,
            font_size_marquee: 18.0,
```

- [ ] **Step 6: Re-baseline the exported templates**

These are what "Download skin…" hands the user, so they must match the defaults exactly.

In `src/skin_templates/dark.css` lines 27-29 and `src/skin_templates/light.css` lines 24-26, preserving the existing column alignment:

```css
    --sp-font-size:          15px;
    --sp-font-size-large:    40px;
    --sp-font-size-marquee:  18px;
```

- [ ] **Step 7: Update the two existing size assertions**

The tests at `src/skin.rs:1528` and `:1566` assert `font-size: 14px` and `font-size: 32px`. With the re-baseline and the unit change these become:

```rust
        assert!(css.contains("font-size: 13.5pt")); // marquee size
```

```rust
        assert!(css.contains("font-size: 30pt")); // large size
```

- [ ] **Step 8: Run the tests to verify they pass**

Run: `distrobox enter dev-box -- sh -c 'cargo test --lib skin:: 2>&1 | tail -25'`
Expected: PASS, including the four new tests.

- [ ] **Step 9: Document the behaviour in the skin guide**

Append to `src/skin_templates/skin-guide.md`, after the font variables are described:

```markdown
### How font sizes scale

Font sizes are declared in `px`, but Sparkamp renders them relative to your
desktop's text size. Turning on large text in GNOME's accessibility settings
scales every size in your skin along with the rest of the desktop.

The numbers are still absolute at default settings — `15px` renders at the
size `15px` suggests — so a skin looks the same on any machine until the
user asks for larger text.

If you want smaller or larger text than the built-in skins use, change these
three values in your own skin file. A skin you have added always wins over
the built-in one, so your sizes are never overridden by a Sparkamp update.
```

- [ ] **Step 10: Run the full suite**

Run: `distrobox enter dev-box -- sh -c 'cargo build && cargo test 2>&1 | tail -20'`
Expected: all pass, zero warnings.

- [ ] **Step 11: Commit**

```bash
git add src/skin.rs src/skin_templates/
git commit -m "feat(skin): render font sizes in pt so they follow the desktop text size

GTK does not scale px, so GNOME's large-text accessibility setting had no
effect on any Sparkamp text. pt is converted through gtk-xft-dpi, which is
exactly what the text-scaling factor multiplies, so the same number renders
at the intended size and grows when the user asks for larger text.

em would also scale, but it resolves against the theme font — so every
existing skin would resize, and the result would depend on whichever font
the user's distribution sets. That breaks the skin guide's promise that one
file renders identically everywhere.

The 14-variable skin format is unchanged: skins still declare px. The
built-in defaults re-baseline to GNOME's native 11pt, which the exported
templates mirror. Custom skins keep their declared sizes, since a user file
wins over the built-in in load_skin."
```

---

## Task 3: Packaging metadata (items 5 and 4)

Removes a third-party trademark from shipped metadata, and completes the metainfo so GNOME Software and Flathub can list the app. No code changes.

**Files:**
- Modify: `packaging/dev.sparkamp.Sparkamp.desktop`
- Modify: `packaging/dev.sparkamp.Sparkamp.metainfo.xml`

**Interfaces:**
- Consumes: `docs/screenshots/README.md` (already committed) for the five filenames.
- Produces: nothing other tasks depend on.

- [ ] **Step 1: Remove the trademark from the desktop entry**

In `packaging/dev.sparkamp.Sparkamp.desktop`, change two lines:

```ini
Comment=Classic-style audio player
Keywords=music;audio;player;mp3;
```

`Comment` drops both "Winamp" and "for Linux" — HIG writing style says not to name the platform back at the user. `Keywords` drops `winamp` and keeps the rest.

- [ ] **Step 2: Remove the trademark from the metainfo summary**

In `packaging/dev.sparkamp.Sparkamp.metainfo.xml`:

```xml
  <summary>Classic-style audio player</summary>
```

And in the `<description>` first paragraph, replace "inspired by the classic Winamp interface" with "with a classic, keyboard-driven interface". Descriptive references to Winamp in `README.md` are fine and stay — this task changes shipped metadata only.

- [ ] **Step 3: Add developer, branding, and form-factor metadata**

In `packaging/dev.sparkamp.Sparkamp.metainfo.xml`, after the `<categories>` block and before `<provides>`:

```xml
  <developer id="dev.sparkamp">
    <name>Sparkamp</name>
  </developer>
  <!-- Sampled from the app icon so the store page's accent matches it. -->
  <branding>
    <color type="primary" scheme_preference="light">#7bc8e8</color>
    <color type="primary" scheme_preference="dark">#00ccff</color>
  </branding>
  <!-- The player window is a fixed 384px wide and is pointer- and
       keyboard-driven; claiming touch or a small form factor would be
       dishonest on a store listing. -->
  <recommends>
    <control>pointer</control>
    <control>keyboard</control>
  </recommends>
  <requires>
    <display_length compare="ge">384</display_length>
  </requires>
```

- [ ] **Step 4: Add the screenshots block**

In the same file, immediately after `<launchable>`:

```xml
  <screenshots>
    <screenshot type="default">
      <image>https://raw.githubusercontent.com/sparkamp/sparkamp/main/docs/screenshots/player.png</image>
      <caption>The main player window</caption>
    </screenshot>
    <screenshot>
      <image>https://raw.githubusercontent.com/sparkamp/sparkamp/main/docs/screenshots/playlist.png</image>
      <caption>The playlist window</caption>
    </screenshot>
    <screenshot>
      <image>https://raw.githubusercontent.com/sparkamp/sparkamp/main/docs/screenshots/media-library.png</image>
      <caption>Browsing the media library</caption>
    </screenshot>
    <screenshot>
      <image>https://raw.githubusercontent.com/sparkamp/sparkamp/main/docs/screenshots/album-gallery.png</image>
      <caption>The album gallery</caption>
    </screenshot>
    <screenshot>
      <image>https://raw.githubusercontent.com/sparkamp/sparkamp/main/docs/screenshots/settings.png</image>
      <caption>Choosing a skin in Settings</caption>
    </screenshot>
  </screenshots>
```

The PNGs do not exist yet — the user captures them during the interactive pass, per `docs/screenshots/README.md`. The block is written now so it is complete the moment the files land.

- [ ] **Step 5: Validate both files**

```bash
distrobox enter dev-box -- sh -c 'appstreamcli validate packaging/dev.sparkamp.Sparkamp.metainfo.xml'
distrobox enter dev-box -- sh -c 'desktop-file-validate packaging/dev.sparkamp.Sparkamp.desktop'
```

Expected: the desktop file validates clean. The metainfo will report missing screenshot images until the PNGs are committed and pushed — that specific warning is expected and is not a failure of this task. Any *other* error must be fixed. If `appstreamcli` is not installed in the box, note it and rely on the XML being well-formed:

```bash
python3 -c "import xml.dom.minidom;xml.dom.minidom.parse('packaging/dev.sparkamp.Sparkamp.metainfo.xml');print('well-formed')"
```

- [ ] **Step 6: Commit**

```bash
git add packaging/dev.sparkamp.Sparkamp.desktop packaging/dev.sparkamp.Sparkamp.metainfo.xml
git commit -m "packaging: drop the Winamp trademark and complete the metainfo

'Winamp' appeared in the metainfo summary, the desktop Comment, and the
desktop Keywords. It is a third-party trademark in shipped metadata:
Flathub review flags it, and it carries legal exposure for no functional
gain. Descriptive prose in the README stays — this is shipped metadata
only.

The metainfo also gained what a store listing needs: screenshots, a
developer, branding colors, and an honest form-factor claim. GNOME
Software and Flathub will not list an app without screenshots, so this is
what actually unblocks distribution. The images themselves are captured
separately; the block references them by name so it completes the moment
they land."
```

---

## Task 4: Standard shortcut aliases (item 8)

Adds Ctrl+F, F1, and Ctrl+? as aliases, and replaces Ctrl+. with Ctrl+, for Settings. Ctrl+Q and Esc keep their current bindings — both were flagged in the audit but are deliberately out of scope for this branch.

**Files:**
- Modify: `frontends/gtk/window/player.rs:9-66` (`shortcut_sections`), `:1415-1430` and `:1469-1490` (the Ctrl wrappers)
- Modify: `frontends/gtk/window/keys.rs`
- Test: `mod tests` at the bottom of `frontends/gtk/window/player.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: nothing other tasks depend on.

- [ ] **Step 1: Write the failing test**

In the existing `mod tests` in `player.rs` (near the `shortcut_dialog_lists_every_phase6_key` guard at line ~2094):

```rust
    /// The shortcuts window is the only place a user discovers these, so a
    /// binding that exists in code but not in the dialog is invisible.
    #[test]
    fn shortcut_dialog_lists_the_standard_aliases() {
        let keys: Vec<&str> = shortcut_sections()
            .iter()
            .flat_map(|(_, entries)| entries.iter().map(|(k, _)| *k))
            .collect();
        let joined = keys.join(" | ");
        for expected in ["Ctrl+F", "F1", "Ctrl+?", "Ctrl+,"] {
            assert!(
                joined.contains(expected),
                "{expected} missing from the shortcuts dialog: {joined}"
            );
        }
    }

    /// Ctrl+. was replaced by Ctrl+, — the GNOME standard, and what the
    /// macOS frontend already uses. Leaving it listed would document a
    /// binding that no longer fires.
    #[test]
    fn shortcut_dialog_no_longer_lists_ctrl_period() {
        let keys: Vec<&str> = shortcut_sections()
            .iter()
            .flat_map(|(_, entries)| entries.iter().map(|(k, _)| *k))
            .collect();
        assert!(
            !keys.iter().any(|k| *k == "Ctrl+."),
            "Ctrl+. is no longer bound and must not be listed"
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `distrobox enter dev-box -- sh -c 'cargo test --bin sparkamp shortcut_dialog 2>&1 | tail -20'`
Expected: FAIL — the aliases are absent and `Ctrl+.` is still listed.

- [ ] **Step 3: Update the shortcuts table**

In `shortcut_sections()` at `player.rs:9`, in the `"View & Tags"` section, replace the `("Ctrl+.", "Open settings")` entry with:

```rust
            ("Ctrl+,",      "Open settings"),
```

In the `"Playlist"` section, after the `("j", "Jump / search")` entry, add:

```rust
            ("Ctrl+F",     "Jump / search"),
```

In the `"Other"` section, replace `("i", "Toggle this help")` with:

```rust
            ("i",          "Toggle this help"),
            ("F1",         "Toggle this help"),
            ("Ctrl+?",     "Toggle this help"),
```

- [ ] **Step 4: Bind Ctrl+, and Ctrl+? in the main-window wrapper**

In `player.rs`, in the Ctrl wrapper at line ~1415, replace the `gdk::Key::period` arm and add the shortcuts arm. `handle_key` is modifier-blind by design, which is why every Ctrl combination lives in these wrappers:

```rust
                    // Ctrl+, → settings. Replaces Ctrl+. (the GNOME standard
                    // is comma, and it is what the macOS frontend binds).
                    gdk::Key::comma => {
                        wrap_open_settings();
                        return glib::Propagation::Stop;
                    }
                    // Ctrl+? → keyboard shortcuts. Arrives as Ctrl+Shift+slash
                    // on most layouts, so both keyvals are accepted.
                    gdk::Key::question | gdk::Key::slash => {
                        wrap_btn_info.emit_clicked();
                        return glib::Propagation::Stop;
                    }
                    // Ctrl+F → jump / search, the reflex shortcut in any list UI.
                    gdk::Key::f | gdk::Key::F => {
                        wrap_open_jump(false);
                        return glib::Propagation::Stop;
                    }
```

Add the two clones this needs alongside the existing `wrap_*` clones above `connect_key_pressed` (line ~1414):

```rust
        let wrap_btn_info = btn_info.clone();
        let wrap_open_jump = open_jump_mode.clone();
```

- [ ] **Step 5: Mirror the same three arms in the playlist-window wrapper**

In the second wrapper at `player.rs:1469`, add the identical `comma`, `question | slash`, and `f | F` arms, with their own clones. Repeating rather than sharing keeps each controller's borrow set independent, which is the pattern the file already uses.

- [ ] **Step 6: Bind Ctrl+F in the Media Library window**

The Media Library has its own search entry that must currently be clicked —
`j` does nothing there. Find the window's `EventControllerKey` in
`media_library.rs` (or add one on the window, with
`PropagationPhase::Capture`, matching the pattern in `player.rs:1409`) and
focus the active page's search entry:

```rust
            if modifier.contains(gdk::ModifierType::CONTROL_MASK)
                && matches!(key, gdk::Key::f | gdk::Key::F)
            {
                // The ML search box otherwise has to be clicked — Ctrl+F is
                // the reflex, and every view here has a search entry.
                ml_search_entry.grab_focus();
                return glib::Propagation::Stop;
            }
```

Resolve `ml_search_entry` to whichever entry belongs to the visible stack
page rather than assuming the Files one — read how the page stack tracks its
current child before wiring this.

- [ ] **Step 7: Bind F1 in the shared dispatcher**

F1 has no modifier, so it belongs in `keys.rs` with the other plain keys. In the `match` in `keys.rs`, next to the `gdk::Key::i | gdk::Key::I` arm at line ~379:

```rust
                // F1 — the HIG binding for Help. Sparkamp has no help manual,
                // so the shortcuts window is the honest target.
                gdk::Key::F1 => {
                    kbd_btn_info.emit_clicked();
                    glib::Propagation::Stop
                }
```

- [ ] **Step 10: Run the tests to verify they pass**

Run: `distrobox enter dev-box -- sh -c 'cargo test --bin sparkamp shortcut 2>&1 | tail -20'`
Expected: PASS, including the pre-existing `shortcut_dialog_lists_every_phase6_key` drift guard.

- [ ] **Step 10: Run the full suite**

Run: `distrobox enter dev-box -- sh -c 'cargo build && cargo test 2>&1 | tail -20'`
Expected: all pass, zero warnings.

- [ ] **Step 10: Commit**

```bash
git add frontends/gtk/window/player.rs frontends/gtk/window/keys.rs
git commit -m "feat(gtk): add the standard GNOME shortcut aliases

Ctrl+F for search, F1 and Ctrl+? for the shortcuts window. All additive:
the Winamp letter keys keep working, and new users find these without
reading the shortcut list first.

Ctrl+, replaces Ctrl+. for Settings. This one is a behavior change rather
than an alias — comma is the GNOME standard and is already what the macOS
frontend binds, so it reduces cross-platform divergence rather than adding
to it. It belongs in the release notes.

Ctrl combinations live in the key-controller wrappers because handle_key
is modifier-blind by design; F1 carries no modifier and so joins the
shared dispatcher."
```

---

## Task 5: Mnemonics on the menu bar and Settings tabs (item 13)

There are zero mnemonics in the tree — `use_underline` and `with_mnemonic` appear nowhere — so the Winamp-style playlist menu bar is mouse-only.

**Files:**
- Modify: `frontends/gtk/window/playlist_window.rs:161-190` (the `menu_button` helper), `:1054-1167` (the four menu labels)
- Modify: `frontends/gtk/window/settings.rs:359, 840, 1399, 2358, 2454` (tab labels)

**Interfaces:**
- Consumes: nothing.
- Produces: nothing other tasks depend on.

- [ ] **Step 1: Teach the menu-button helper to accept mnemonics**

In `playlist_window.rs`, the `menu_button` helper builds a `MenuButton` and sets its label. Find where it applies the label to the button (around line 183, after `let mb = gtk4::MenuButton::new();`) and make the label mnemonic-aware:

```rust
        let mb = gtk4::MenuButton::new();
        // use_underline makes `_A` an Alt+A access key. GTK shows the
        // underline only while Alt is held, so the menu bar looks unchanged.
        mb.set_use_underline(true);
```

If the helper sets the label via `mb.set_label(label)`, that call already honours `use_underline` once the property is set — no further change is needed there.

- [ ] **Step 2: Add the access keys to the four menu labels**

At `playlist_window.rs:1054`, `:1074`, `:1116`, and `:1142`, change the first argument of each `menu_button(...)` call. Sort takes `o` because `S` is already claimed by Select within the same container:

```rust
    let add_menu = menu_button(
        "_Add ▾",
```

```rust
    let select_menu = menu_button(
        "_Select ▾",
```

```rust
    let sort_menu = menu_button(
        "S_ort ▾",
```

```rust
    let list_menu = menu_button(
        "_List ▾",
```

- [ ] **Step 3: Add access keys to the Settings tabs**

In `settings.rs`, the five tab labels are built with `Label::new(Some("..."))`. A plain `Label` ignores underscores, so each must switch to the mnemonic constructor. At lines 359, 840, 1399, 2358, and 2454 respectively:

```rust
        let tab_lbl = Label::with_mnemonic("_Appearance");
```

```rust
        let tab_lbl = Label::with_mnemonic("_Behavior");
```

```rust
        let tab_lbl = Label::with_mnemonic("_Visualizer");
```

```rust
        let tab_lbl = Label::with_mnemonic("_Media Library");
```

```rust
        let tab_lbl = Label::with_mnemonic("A_bout");
```

About takes `b` because `A` is claimed by Appearance in the same notebook.

- [ ] **Step 4: Build and run the suite**

Run: `distrobox enter dev-box -- sh -c 'cargo build && cargo test 2>&1 | tail -20'`
Expected: all pass, zero warnings.

- [ ] **Step 5: Commit**

```bash
git add frontends/gtk/window/playlist_window.rs frontends/gtk/window/settings.rs
git commit -m "feat(gtk): add access keys to the menu bar and Settings tabs

The Winamp-style playlist menu bar was mouse-only — there were no
mnemonics anywhere in the tree. Alt+A/S/O/L now reach the four menus and
Alt+A/B/V/M/B the Settings tabs.

Access keys are deconflicted within each container: Sort takes 'o'
because Select claims 'S', and About takes 'b' because Appearance claims
'A'. Nothing changes visually — GTK draws the underline only while Alt is
held."
```

---

## Task 6: TUI shortcut consistency (item 14)

`/` currently **clears the entire playlist** in the playlist view while meaning **search** in the Media Library. Same app, same key, one destructive — and it is the key every terminal user reflexively presses to search. This task fixes that, and adds the two aliases that make sense in a terminal.

Ctrl+? and Ctrl+, are deliberately **not** attempted: Ctrl+? *is* `0x7F`, the DEL byte, indistinguishable from Backspace without the kitty keyboard protocol, and Ctrl+, has no ASCII encoding at all.

**Files:**
- Modify: `frontends/tui/keys.rs:499` (the `/` arm), `:835-843` (`PLAYLIST_OPS_LABELS`), `:853-888` (`handle_playlist_ops`), `:509` (help arm)
- Modify: `frontends/tui/ui/overlays.rs` (help overlay text)
- Test: `frontends/tui/tests/` or `mod tests` in `frontends/tui/mod.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `PLAYLIST_OPS_LABELS` grows from 7 to 8 entries; `handle_playlist_ops` gains index 7.

- [ ] **Step 1: Write the failing tests**

In `frontends/tui/tests/keys_input.rs` (which already exercises `handle_key`), add:

```rust
    /// `/` used to clear the entire playlist while meaning "search" in the
    /// Media Library. It is the key every terminal user presses to search,
    /// so the destructive binding was a data-loss trap.
    #[test]
    fn slash_opens_search_and_does_not_clear_the_playlist() {
        let mut app = make_app_with_tracks(3);
        app.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
        assert_eq!(app.playlist.len(), 3, "/ must not clear the playlist");
        assert!(matches!(app.mode, Mode::Jump { .. }), "/ must open search");
    }

    /// Ctrl+F is the same action, matching what the Media Library already
    /// accepts.
    #[test]
    fn ctrl_f_opens_search() {
        let mut app = make_app_with_tracks(3);
        app.handle_key(KeyCode::Char('f'), KeyModifiers::CONTROL);
        assert!(matches!(app.mode, Mode::Jump { .. }));
    }

    /// Clearing the playlist is still reachable, but from the ops popup
    /// where the other whole-playlist operations live — the same place
    /// GTK puts it, as List ▾ ▸ Remove All.
    #[test]
    fn remove_all_is_reachable_from_the_ops_popup() {
        let mut app = make_app_with_tracks(3);
        app.handle_key(KeyCode::Char('o'), KeyModifiers::NONE);
        let idx = App::PLAYLIST_OPS_LABELS
            .iter()
            .position(|l| *l == "Remove All")
            .expect("ops popup must offer Remove All");
        for _ in 0..idx {
            app.handle_key(KeyCode::Down, KeyModifiers::NONE);
        }
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.playlist.len(), 0);
    }

    /// F1 joins `i` for help. No F-keys were bound before, so there is no
    /// conflict; `i` still works if a multiplexer intercepts F1.
    #[test]
    fn f1_opens_help() {
        let mut app = make_app_with_tracks(1);
        app.handle_key(KeyCode::F(1), KeyModifiers::NONE);
        assert!(matches!(app.mode, Mode::Help { .. }));
    }
```

If `make_app_with_tracks` does not exist in that test file, use the existing `make_app()` helper described in `CLAUDE.md` and push tracks onto `app.playlist` directly.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `distrobox enter dev-box -- sh -c 'cargo test --bin sparkamp keys_input 2>&1 | tail -25'`
Expected: FAIL — `/` still clears, Ctrl+F and F1 are unbound, "Remove All" is not in the labels.

- [ ] **Step 3: Add Remove All to the ops popup**

In `frontends/tui/keys.rs`, change the constant at line 835 from 7 to 8 entries:

```rust
    pub(super) const PLAYLIST_OPS_LABELS: [&'static str; 8] = [
        "Sort: Title",
        "Sort: Artist",
        "Sort: Album",
        "Sort: Filename",
        "Sort: Path",
        "Randomize",
        "Reverse",
        "Remove All",
    ];
```

In `handle_playlist_ops`, add the matching arm inside the `KeyCode::Enter` match (after `6 => self.playlist_reverse(),`):

```rust
                    7 => {
                        // Clearing the whole playlist belongs with the other
                        // whole-playlist operations, not on a bare keystroke.
                        // Mirrors GTK's List ▾ ▸ Remove All.
                        let _ = self.player.stop();
                        self.playlist.tracks.clear();
                        self.playlist.current_index = 0;
                        self.playlist_cursor = 0;
                        self.shuffle_state.reset();
                        self.set_status("Playlist cleared");
                    }
```

- [ ] **Step 4: Repoint `/` at search**

In `keys.rs`, replace the whole `KeyCode::Char('/')` arm at line 499 (the one that clears the playlist) with a search opener that matches what `j` already does:

```rust
            // '/' — open jump / search. Every terminal app binds '/' to
            // search, and the Media Library already does. This used to clear
            // the entire playlist, which was a data-loss trap on the key
            // users press to search; Remove All now lives in the `o` popup.
            KeyCode::Char('/') => {
                self.open_jump(false);
            }
```

Use whatever the `j` arm at line 445 calls — if `j` inlines its mode change rather than calling a helper, copy that body verbatim rather than inventing `open_jump`.

- [ ] **Step 5: Bind Ctrl+F and F1**

`handle_key` receives `modifiers`, and the existing Ctrl+Q check at line 219 shows the pattern. Add alongside it, before the plain-key match:

```rust
        // Ctrl+F — search, matching the Media Library's existing binding.
        if modifiers.contains(KeyModifiers::CONTROL)
            && matches!(code, KeyCode::Char('f') | KeyCode::Char('F'))
        {
            self.open_jump(false);
            return;
        }
```

And in the plain-key match, next to the `i`/`I` help arm at line 509:

```rust
            // F1 — help, alongside `i`. No F-keys were bound before this, so
            // there is no conflict; `i` still works if a terminal multiplexer
            // swallows F1.
            KeyCode::F(1) => {
                self.mode = Mode::Help { scroll: 0 };
            }
```

- [ ] **Step 6: Update the help overlay text**

In `frontends/tui/ui/overlays.rs`, in the help lines (around line 332 onward), add the new bindings and correct the `/` description. Match the surrounding `Line::from(vec![key("  x"), Span::raw("      ...")])` style and column alignment exactly:

```rust
        Line::from(vec![key("  /"), Span::raw("      Jump / search")]),
        Line::from(vec![key("  Ctrl+F"), Span::raw(" Jump / search")]),
        Line::from(vec![key("  F1"), Span::raw("     Toggle this help")]),
```

Place the first two next to the existing `j` entry and the third next to the existing `i` entry. If a line documenting `/` as "Clear playlist" exists, remove it — the ops popup entry covers it now.

- [ ] **Step 7: Run the tests to verify they pass**

Run: `distrobox enter dev-box -- sh -c 'cargo test --bin sparkamp 2>&1 | tail -25'`
Expected: PASS. Any pre-existing test asserting that `/` clears the playlist is now wrong by design — update it to assert the new behaviour rather than deleting it.

- [ ] **Step 8: Run the full suite**

Run: `distrobox enter dev-box -- sh -c 'cargo build && cargo test 2>&1 | tail -20'`
Expected: all pass, zero warnings.

- [ ] **Step 9: Commit**

```bash
git add frontends/tui/
git commit -m "fix(tui): stop '/' clearing the playlist; bind it to search

'/' cleared the entire playlist in the playlist view while meaning
'search' in the Media Library. Same app, same key, one of them
destructive — and it is the key every terminal user reflexively presses
to search. That is a data-loss trap, not a style nit.

'/' and Ctrl+F now open jump/search in both places, matching what the
Media Library already accepted. Clearing moves into the 'o' ops popup as
Remove All, next to the other whole-playlist operations and mirroring
GTK's List ▾ ▸ Remove All. F1 joins 'i' for help.

Ctrl+? and Ctrl+, are deliberately absent: Ctrl+? is the DEL byte and
Ctrl+, has no ASCII encoding, so neither survives a terminal without the
kitty keyboard protocol. This is terminal convention, not HIG — the HIG
does not govern TUIs, and the vim/less idiom here is already correct."
```

---

## Task 7: Track-change notification (item 9)

The GTK frontend sends no desktop notifications. MPRIS already publishes metadata, so GNOME Shell shows Sparkamp in its media section; what is missing is the transient banner on track change.

**Files:**
- Modify: `src/model.rs` (a testable `notification_lines` helper on `Track`)
- Modify: `src/config.rs:116-142` (`PlaybackConfig`), `:984-992` (`Config::default`)
- Modify: `frontends/gtk/window/player.rs` (the subscriber)
- Modify: `frontends/gtk/window/settings.rs` (Behavior tab checkbox)
- Test: `mod tests` in `src/model.rs` and `src/config.rs`

**Interfaces:**
- Consumes: `AppState::subscribe_now_playing` (`state.rs:831`).
- Produces: `Track::notification_lines(&self) -> (String, Option<String>)`; `PlaybackConfig::notify_track_change: bool`.

- [ ] **Step 1: Write the failing core tests**

In `mod tests` in `src/model.rs`:

```rust
    #[test]
    fn notification_lines_uses_title_and_artist() {
        let t = Track {
            path: PathBuf::from("/fake/song.mp3"),
            title: "Dark Horse".into(),
            artist: "Katy Perry".into(),
            album_artist: String::new(),
            album: String::new(),
            duration: None,
            broken: false,
        };
        assert_eq!(
            t.notification_lines(),
            ("Dark Horse".to_string(), Some("Katy Perry".to_string()))
        );
    }

    /// Same TPE1-then-TPE2 precedence as display_name, so the banner and the
    /// marquee never disagree about who the artist is.
    #[test]
    fn notification_lines_falls_back_to_album_artist() {
        let t = Track {
            path: PathBuf::from("/fake/song.mp3"),
            title: "Song".into(),
            artist: String::new(),
            album_artist: "Various Artists".into(),
            album: String::new(),
            duration: None,
            broken: false,
        };
        assert_eq!(t.notification_lines().1, Some("Various Artists".to_string()));
    }

    /// An untagged file has no artist at all. An empty second line reads as
    /// a rendering bug, so the body is omitted rather than blank.
    #[test]
    fn notification_lines_omits_an_empty_body() {
        let t = Track {
            path: PathBuf::from("/fake/song.mp3"),
            title: "song".into(),
            artist: String::new(),
            album_artist: String::new(),
            album: String::new(),
            duration: None,
            broken: false,
        };
        assert_eq!(t.notification_lines(), ("song".to_string(), None));
    }
```

And in `mod tests` in `src/config.rs`:

```rust
    /// New fields must carry #[serde(default)] so a config written by an
    /// older build still loads.
    #[test]
    fn playback_config_without_notify_field_loads() {
        let older = "volume = 0.8\nstart_paused = false\n";
        let back: PlaybackConfig = toml::from_str(older).expect("pre-notify config loads");
        assert!(back.notify_track_change, "default is on");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `distrobox enter dev-box -- sh -c 'cargo test --lib notification_lines playback_config_without 2>&1 | tail -20'`
Expected: FAIL — neither the method nor the field exists.

- [ ] **Step 3: Add the core helper**

In `src/model.rs`, next to `display_name` (line ~236):

```rust
    /// The (heading, body) pair a desktop notification shows for this track.
    ///
    /// Uses the same TPE1-then-TPE2 artist precedence as [`display_name`], so
    /// the banner and the marquee never disagree about who the artist is.
    /// The body is `None` rather than empty when neither tag is present — an
    /// empty second line in a notification reads as a rendering bug.
    pub fn notification_lines(&self) -> (String, Option<String>) {
        let artist = if !self.artist.is_empty() {
            self.artist.as_str()
        } else {
            self.album_artist.as_str()
        };
        let body = if artist.is_empty() {
            None
        } else {
            Some(artist.to_string())
        };
        (self.title.clone(), body)
    }
```

- [ ] **Step 4: Add the config field**

In `src/config.rs`, at the end of `PlaybackConfig` (before the closing brace at line ~142):

```rust
    /// Whether a desktop notification is posted on track change. Only fires
    /// when no Sparkamp window is focused — a banner over the player you are
    /// already looking at is why people turn music notifications off.
    #[serde(default = "default_notify_track_change")]
    pub notify_track_change: bool,
```

After the struct, next to `default_fadeout_secs`:

```rust
fn default_notify_track_change() -> bool {
    true
}
```

And in `Config::default()` at line ~984, inside the `PlaybackConfig { .. }` literal:

```rust
                notify_track_change: true,
```

- [ ] **Step 5: Run the core tests to verify they pass**

Run: `distrobox enter dev-box -- sh -c 'cargo test --lib notification_lines playback_config_without 2>&1 | tail -20'`
Expected: PASS.

- [ ] **Step 6: Post the notification from the GTK frontend**

In `player.rs`, after the existing `subscribe_now_playing` registrations (around line 718-736), add another subscriber. It needs the `Application` and the two windows to test focus:

```rust
    // Desktop notification on track change. MPRIS already publishes metadata,
    // so the Shell's media widget is covered; this is the transient banner.
    // Fires only when no Sparkamp window is focused — a banner over the
    // player you are already looking at is why people disable these.
    {
        let state_rc = state.clone();
        let app_rc = app.clone();
        let win_wk = window.downgrade();
        let pl_wk = playlist_win.downgrade();
        let cb: Rc<dyn Fn(&crate::now_playing::NowPlayingInfo)> = Rc::new(move |_info| {
            if !state_rc.borrow().config.playback.notify_track_change {
                return;
            }
            let focused = win_wk.upgrade().map(|w| w.is_active()).unwrap_or(false)
                || pl_wk.upgrade().map(|w| w.is_active()).unwrap_or(false);
            if focused {
                return;
            }
            let (heading, body) = {
                let s = state_rc.borrow();
                match s.playlist.tracks.get(s.playlist.current_index) {
                    Some(t) => t.notification_lines(),
                    None => return,
                }
            };
            let n = gio::Notification::new(&gtk_safe(&heading));
            if let Some(b) = body {
                n.set_body(Some(&gtk_safe(&b)));
            }
            // The app icon rather than the cover: a notification icon is
            // rendered at ~48px, where album art is unreadable anyway, and
            // this keeps the banner identifiably Sparkamp's.
            n.set_icon(&gio::ThemedIcon::new("dev.sparkamp.Sparkamp"));
            // A stable id replaces the previous banner instead of stacking
            // one per track.
            app_rc.send_notification(Some("sparkamp-track"), &n);
        });
        state.borrow_mut().subscribe_now_playing(cb);
    }
```

If `playlist_win` is not yet in scope at the chosen insertion point, place this block after its construction at line ~1057.

- [ ] **Step 7: Add the Settings checkbox**

In `settings.rs`, on the Behavior tab (the block beginning at line 363), following the `chk_autocd` pattern at line 461:

```rust
        let chk_notify = CheckButton::with_label("Show a notification when the track changes");
        chk_notify.set_tooltip_text(Some(
            "Only while no Sparkamp window is focused",
        ));
        chk_notify.set_active(state.borrow().config.playback.notify_track_change);
        {
            let state_rc = state.clone();
            chk_notify.connect_toggled(move |c| {
                let mut s = state_rc.borrow_mut();
                s.config.playback.notify_track_change = c.is_active();
                let _ = s.config.save();
            });
        }
```

Attach it to the Behavior tab's grid at the next free row, following the `grid.attach(&chk_autocd, 1, 3, 1, 1);` idiom — read the surrounding rows and use the next index rather than assuming one.

- [ ] **Step 8: Run the full suite**

Run: `distrobox enter dev-box -- sh -c 'cargo build && cargo test 2>&1 | tail -20'`
Expected: all pass, zero warnings.

- [ ] **Step 9: Commit**

```bash
git add src/model.rs src/config.rs frontends/gtk/window/player.rs frontends/gtk/window/settings.rs
git commit -m "feat(gtk): notify on track change while the app is in the background

A background music player that never says what started playing is the one
thing every other player does. MPRIS already feeds the Shell's media
widget; what was missing is the transient banner.

It fires only when no Sparkamp window is focused. A banner over the player
you are already looking at is the reason people disable music
notifications, and the HIG asks apps not to interrupt without cause. A
Settings toggle turns it off entirely.

The title/artist split lives in core as Track::notification_lines so it is
unit-testable and shares display_name's artist precedence — the banner and
the marquee never disagree about who the artist is.

No new Flatpak permission: the portal talk-name is already granted."
```

---

## Task 8: Toasts for non-fatal feedback (item 10)

Seventeen `AlertDialog` call sites stop the user for things that do not warrant stopping. The codebase already documents the right instinct — "G3: no success modals anywhere" (`util.rs:249`) — but failures still go modal.

**Files:**
- Modify: `frontends/gtk/window/util.rs` (new helper beside the existing alert helpers)
- Modify: `frontends/gtk/window/player.rs` (wrap the main and playlist window roots)
- Modify: `frontends/gtk/window/media_library.rs` (wrap the ML window root)
- Modify: call sites listed in Step 5

**Interfaces:**
- Consumes: `adw` from Task 1.
- Produces: `pub(super) fn show_toast(win: &gtk4::Window, msg: &str)` and `pub(super) fn toast_overlay_for(win: &gtk4::Window) -> Option<adw::ToastOverlay>` in `util.rs`.

- [ ] **Step 1: Wrap each window root in a ToastOverlay**

A toast needs an `AdwToastOverlay` ancestor. Rather than thread one through every call site, each top-level window gets its root wrapped once, and the helper walks up to find it.

In `player.rs`, immediately after `let root = GtkBox::new(Orientation::Vertical, 0);` (line ~384) is built out and just before it is set as the window child, wrap it:

```rust
    // Every toast in this window lands here. Wrapping the root once means
    // call sites only need the window, not a threaded-through overlay.
    let toaster = adw::ToastOverlay::new();
    toaster.set_child(Some(&root));
    window.set_child(Some(&toaster));
```

Replace whatever `window.set_child(Some(&root))` call currently exists. Do the same for the playlist window's root and for the Media Library window in `media_library.rs` (its root is the `paned` at line ~171).

- [ ] **Step 2: Add the helpers**

In `util.rs`, next to `show_alert_parented` (line ~223):

```rust
/// Find the `AdwToastOverlay` wrapping a window's content, if it has one.
///
/// Every top-level window that can raise a toast wraps its root in one at
/// construction, so this is a single downcast rather than a tree walk.
pub(super) fn toast_overlay_for(win: &gtk4::Window) -> Option<adw::ToastOverlay> {
    win.child().and_downcast::<adw::ToastOverlay>()
}

/// Show a transient message at the bottom of `win`.
///
/// This is the non-fatal path: a recoverable failure the user can act on
/// later, or not at all. Modal alerts stay for destructive confirmations and
/// for errors that must be acknowledged before anything else can proceed.
///
/// Falls back to a modal alert when the window has no overlay, so a caller
/// can never silently drop a message it believed it had shown.
pub(super) fn show_toast(win: &gtk4::Window, msg: &str) {
    match toast_overlay_for(win) {
        Some(overlay) => overlay.add_toast(adw::Toast::new(&gtk_safe(msg))),
        None => show_alert_parented(Some(win), msg),
    }
}
```

- [ ] **Step 3: Write the failing test**

In `mod tests` in `frontends/gtk/window/tests.rs`:

```rust
    /// A window whose root is wrapped resolves its overlay; one that is not
    /// wrapped resolves None, which is what makes show_toast's fallback to a
    /// modal alert reachable rather than dead code.
    #[test]
    fn toast_overlay_is_found_only_when_the_root_is_wrapped() {
        crate::tests::init_gtk();
        let bare = gtk4::Window::new();
        bare.set_child(Some(&gtk4::Box::new(gtk4::Orientation::Vertical, 0)));
        assert!(super::util::toast_overlay_for(&bare).is_none());

        let wrapped = gtk4::Window::new();
        let overlay = adw::ToastOverlay::new();
        overlay.set_child(Some(&gtk4::Box::new(gtk4::Orientation::Vertical, 0)));
        wrapped.set_child(Some(&overlay));
        assert!(super::util::toast_overlay_for(&wrapped).is_some());
    }
```

Use whatever GTK-init helper `tests.rs` already uses for widget tests; if there is none, call `gtk4::init().ok();` at the top of the test as the file's other widget tests do.

- [ ] **Step 4: Run the test to verify it fails, then passes**

Run: `distrobox enter dev-box -- sh -c 'cargo test --bin sparkamp toast_overlay 2>&1 | tail -20'`
Expected: FAIL before Step 2's helper exists, PASS after.

- [ ] **Step 5: Demote the non-fatal alerts**

Convert these three helpers' call sites. Each currently builds a modal `AlertDialog`; each is a recoverable failure the user does not need to acknowledge before continuing.

- `show_playlist_save_error` (`util.rs:730`) — a failed playlist write. Change its body to call `show_toast` with a one-line message, keeping the target path:

```rust
pub(super) fn show_playlist_save_error(parent: &gtk4::Window, target: &std::path::Path, err: &anyhow::Error) {
    // Non-fatal: the playlist is intact in memory and the user can retry or
    // pick another location. A modal here interrupted a save they can simply
    // do again.
    show_toast(
        parent,
        &format!("Couldn't save {}: {err}", target.display()),
    );
}
```

- `show_unreadable_dialog` (`util.rs:234`) — files skipped during an add. Keep this one **modal**: it enumerates a list the user needs to read and act on, which a toast cannot hold.

- `show_alert_parented` (`util.rs:223`) — audit its callers with `grep -rn "show_alert_parented" frontends/gtk/`. Convert callers reporting a recoverable single-item failure (artwork decode, a tag write that failed, a device that vanished mid-operation) to `show_toast`. Leave callers that gate a destructive action or report an unrecoverable state as modal alerts. Record which callers moved in the commit message.

- [ ] **Step 6: Run the full suite**

Run: `distrobox enter dev-box -- sh -c 'cargo build && cargo test 2>&1 | tail -20'`
Expected: all pass, zero warnings.

- [ ] **Step 7: Commit**

```bash
git add frontends/gtk/window/
git commit -m "feat(gtk): show recoverable failures as toasts, not modal alerts

Seventeen AlertDialog sites stopped the user for things that did not
warrant stopping. The file already documented the right instinct — 'G3: no
success modals anywhere' — but failures still went modal.

Each top-level window wraps its root in an AdwToastOverlay once, so call
sites need only the window. show_toast falls back to a modal alert when a
window has no overlay, so a caller can never silently drop a message it
believed it had shown.

Destructive confirmations and unrecoverable errors stay modal, as does the
unreadable-files report — it enumerates a list the user has to read, which
a toast cannot hold."
```

---

## Task 9: Empty states (item 12)

Blank panes today. `AdwStatusPage` behind one helper, so copy and icon are the only per-site variation.

**Files:**
- Modify: `frontends/gtk/window/util.rs` (helper)
- Modify: `frontends/gtk/window/playlist_window.rs` (active playlist)
- Modify: `frontends/gtk/window/files.rs:974-982` (Files view and its no-results state)
- Modify: `frontends/gtk/window/album_gallery.rs`, `playlists.rs`, `devices_page.rs`, `disc_data.rs`

**Interfaces:**
- Consumes: `adw` from Task 1.
- Produces: `pub(super) fn empty_state(icon: &str, heading: &str, body: Option<&str>) -> adw::StatusPage` and `pub(super) fn stack_with_empty_state(content: &impl IsA<gtk4::Widget>, empty: &adw::StatusPage) -> gtk4::Stack` in `util.rs`.

- [ ] **Step 1: Add the helpers**

In `util.rs`:

```rust
/// Build a placeholder page for an empty view.
///
/// `icon` is a symbolic icon name; the HIG asks for a subtle monochrome
/// icon in secondary spaces rather than an illustration.
pub(super) fn empty_state(icon: &str, heading: &str, body: Option<&str>) -> adw::StatusPage {
    let page = adw::StatusPage::new();
    page.set_icon_name(Some(icon));
    page.set_title(&gtk_safe(heading));
    if let Some(b) = body {
        page.set_description(Some(&gtk_safe(b)));
    }
    page
}

/// Put `content` and `empty` in a stack so a view can swap between them.
///
/// The caller drives the swap from whatever signals its model emits —
/// usually `items_changed` on the backing `ListStore`, the same seam the
/// Media Library status bars already use.
pub(super) fn stack_with_empty_state(
    content: &impl gtk4::glib::IsA<gtk4::Widget>,
    empty: &adw::StatusPage,
) -> gtk4::Stack {
    let stack = gtk4::Stack::new();
    stack.add_named(content, Some("content"));
    stack.add_named(empty, Some("empty"));
    stack.set_visible_child_name("empty");
    stack
}
```

- [ ] **Step 2: Write the failing test**

In `tests.rs`:

```rust
    /// The stack starts on the empty page: a view is empty until its model
    /// says otherwise, and starting on "content" would flash a blank table
    /// on every open.
    #[test]
    fn empty_state_stack_starts_empty_and_can_swap() {
        crate::tests::init_gtk();
        let content = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        let page = super::util::empty_state("folder-music-symbolic", "No music folders", None);
        let stack = super::util::stack_with_empty_state(&content, &page);
        assert_eq!(stack.visible_child_name().as_deref(), Some("empty"));
        stack.set_visible_child_name("content");
        assert_eq!(stack.visible_child_name().as_deref(), Some("content"));
    }
```

- [ ] **Step 3: Run the test to verify it fails, then passes**

Run: `distrobox enter dev-box -- sh -c 'cargo test --bin sparkamp empty_state 2>&1 | tail -20'`
Expected: FAIL, then PASS once Step 1 lands.

- [ ] **Step 4: Wire the Files view**

In `files.rs`, the `track_scroll` built at line 974 is appended to `files_vbox` at line 982. Wrap it:

```rust
        // Two empty states share this view: nothing indexed at all, and a
        // search that matched nothing. The second is only reachable once the
        // first is not, so one stack with a swapped-out page covers both.
        let files_empty = super::util::empty_state(
            "folder-music-symbolic",
            "No music folders",
            Some("Add a folder to start building your library"),
        );
        let files_stack = super::util::stack_with_empty_state(&track_scroll, &files_empty);
        files_vbox.append(&files_stack);
```

Then drive the swap from the store. Next to wherever `track_store` is populated, add:

```rust
        {
            let stack = files_stack.clone();
            let empty = files_empty.clone();
            let entry = search_entry.clone();
            track_store.connect_items_changed(move |store, _, _, _| {
                if store.n_items() > 0 {
                    stack.set_visible_child_name("content");
                    return;
                }
                let q = entry.text();
                if q.is_empty() {
                    empty.set_icon_name(Some("folder-music-symbolic"));
                    empty.set_title("No music folders");
                    empty.set_description(Some("Add a folder to start building your library"));
                } else {
                    empty.set_icon_name(Some("system-search-symbolic"));
                    empty.set_title("No results");
                    empty.set_description(Some(&format!("Nothing matches “{q}”")));
                }
                stack.set_visible_child_name("empty");
            });
        }
```

Note the typographic quotation marks (U+201C/U+201D) — the HIG typography guidance asks for those rather than straight quotes.

- [ ] **Step 5: Wire the active playlist**

In `playlist_window.rs`, wrap the `TreeView`'s `ScrolledWindow` the same way. This is the state a new user sees first, so it doubles as onboarding:

```rust
    let pl_empty = super::util::empty_state(
        "view-list-symbolic",
        "No tracks in the playlist",
        Some("Press n to add files, or drag music here"),
    );
```

Drive the swap wherever `rebuild_playlist` finishes, using `state.borrow().playlist.tracks.is_empty()`.

- [ ] **Step 6: Wire the remaining four views**

Repeat the Step 4 pattern in each, with the same `items_changed` seam and this copy:

- `album_gallery.rs` — icon `media-optical-symbolic`, "No albums", "Albums appear here once your library has tagged music"
- `playlists.rs` (saved playlists list) — icon `view-list-symbolic`, "No saved playlists", "Save the current playlist to see it here"
- `devices_page.rs` — icon `drive-removable-media-symbolic`, "No devices connected", "Connect a music player or USB drive"
- `disc_data.rs` — icon `media-optical-symbolic`, "No disc inserted", "Insert an audio CD or data disc"

Each also gets the no-results variant from Step 4 where the view has a search entry.

- [ ] **Step 7: Run the full suite**

Run: `distrobox enter dev-box -- sh -c 'cargo build && cargo test 2>&1 | tail -20'`
Expected: all pass, zero warnings.

- [ ] **Step 8: Commit**

```bash
git add frontends/gtk/window/
git commit -m "feat(gtk): give every empty view a placeholder page

Six views rendered as a blank pane when they had nothing to show, which
reads as a broken app rather than an empty one. The active playlist's
state doubles as onboarding — it is the first thing a new user sees.

One helper builds the page and one puts it in a stack with the real
content; each view drives the swap from its own model's items_changed,
the same seam the Media Library status bars already use. Views with a
search entry distinguish 'nothing here yet' from 'nothing matched', which
the HIG asks for specifically.

AdwStatusPage does not read the skin variables, so these will read as
stock-GNOME panels inside a skinned window. That is the drift to judge
during the interactive pass; the fallback is a hand-built skin-styled box
behind the same helper signature."
```

---

## Task 10: Accessible names on controls (item 1, part 1)

There are zero accessibility API calls in the 37,557 lines of `frontends/gtk/`. Every icon-only control announces as "button" or as nothing.

**Files:**
- Modify: `frontends/gtk/window/player.rs` (transport, mode buttons, scales, visualizer)
- Modify: `frontends/gtk/window/eq.rs` (the ten band scales and pre-amp)
- Modify: `frontends/gtk/window/now_playing.rs`, `art_window.rs`, `viz.rs` (drawing areas)
- Modify: per-view action buttons in `files.rs`, `devices_page.rs`, `disc_page.rs`, `playlists_manage.rs`
- Test: `mod tests` in `frontends/gtk/window/tests.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: nothing other tasks depend on.

- [ ] **Step 1: Write the failing test**

In `tests.rs`:

```rust
    /// A screen reader announces an icon-only button by its accessible name.
    /// Without one it says "button", which is the state the whole frontend
    /// was in before this.
    #[test]
    fn icon_only_buttons_carry_an_accessible_label() {
        crate::tests::init_gtk();
        let b = gtk4::Button::from_icon_name("media-playback-start-symbolic");
        b.update_property(&[gtk4::accessible::Property::Label("Play")]);
        assert_eq!(
            b.accessible_property(gtk4::AccessibleProperty::Label)
                .map(|v| v.get::<String>().unwrap_or_default())
                .unwrap_or_default(),
            "Play"
        );
    }
```

If `accessible_property` is not exposed as a getter in gtk4-rs 0.9, drop the assertion and keep the test as a compile-level guard that `update_property` accepts the property — note the substitution in the commit message rather than inventing an API.

- [ ] **Step 2: Run the test**

Run: `distrobox enter dev-box -- sh -c 'cargo test --bin sparkamp accessible 2>&1 | tail -20'`
Expected: PASS or a compile error naming the unavailable getter — resolve per Step 1's note.

- [ ] **Step 3: Label the transport buttons**

In `player.rs` after lines 966-970:

```rust
    // Icon-only buttons announce as "button" without this. The label is what
    // a screen reader reads, so it is the control's name, not its shortcut.
    btn_prev.update_property(&[gtk4::accessible::Property::Label("Previous track")]);
    btn_play.update_property(&[gtk4::accessible::Property::Label("Play")]);
    btn_pause.update_property(&[gtk4::accessible::Property::Label("Pause")]);
    btn_stop.update_property(&[gtk4::accessible::Property::Label("Stop")]);
    btn_next.update_property(&[gtk4::accessible::Property::Label("Next track")]);
```

- [ ] **Step 4: Label the mode buttons**

After lines 786-839:

```rust
    btn_pl.update_property(&[gtk4::accessible::Property::Label("Playlist")]);
    btn_eq.update_property(&[gtk4::accessible::Property::Label("Equalizer")]);
    btn_ml.update_property(&[gtk4::accessible::Property::Label("Media library")]);
    btn_info.update_property(&[gtk4::accessible::Property::Label("Keyboard shortcuts")]);
    btn_jump_vol.update_property(&[gtk4::accessible::Property::Label("Jump to track")]);
    btn_repeat.update_property(&[gtk4::accessible::Property::Label("Repeat mode")]);
    btn_shuffle.update_property(&[gtk4::accessible::Property::Label("Shuffle")]);
    np_toggle.update_property(&[gtk4::accessible::Property::Label("Now playing panel")]);
```

- [ ] **Step 5: Label the sliders, including live value text**

A bare `Scale` announces its raw number. `ValueText` is what turns "73" into something meaningful:

```rust
    seek_bar.update_property(&[gtk4::accessible::Property::Label("Seek")]);
    vol_scale.update_property(&[gtk4::accessible::Property::Label("Volume")]);
```

The seek bar's `ValueText` must follow playback, so update it where the tick loop already writes the time display. In `tick.rs`, next to the existing elapsed/remaining label update:

```rust
            // Keep the spoken value in step with the visible one — a screen
            // reader otherwise reads the raw slider position.
            seek_bar.update_property(&[gtk4::accessible::Property::ValueText(&time_text)]);
```

Use whatever variable already holds the formatted "1:23 / 4:05" string at that point rather than recomputing it.

In `eq.rs`, label the ten band scales and the pre-amp by their frequency, reading the band labels the file already builds rather than hard-coding a second list.

- [ ] **Step 6: Label the drawing areas**

A `DrawingArea` has no implicit name. In `viz.rs`, `now_playing.rs`, and `art_window.rs`:

```rust
    viz_area.update_property(&[
        gtk4::accessible::Property::Label("Visualizer"),
        gtk4::accessible::Property::Description(
            "A decorative animation of the audio being played",
        ),
    ]);
```

Album art areas take `Label("Album art")` plus a `Description` carrying the album name when one is known.

- [ ] **Step 7: Sweep the per-view action buttons**

Find the remaining icon-only buttons and label each by what it does:

```bash
grep -rn "from_icon_name" frontends/gtk/window/ | grep -v "Image::from_icon_name"
```

Every hit that is a `Button` needs a label. `Image::from_icon_name` results are decorative and do not.

- [ ] **Step 8: Run the full suite**

Run: `distrobox enter dev-box -- sh -c 'cargo build && cargo test 2>&1 | tail -20'`
Expected: all pass, zero warnings.

- [ ] **Step 9: Commit**

```bash
git add frontends/gtk/
git commit -m "feat(gtk): give every icon-only control an accessible name

There were zero accessibility API calls in the GTK frontend. A screen
reader announced the entire transport row as 'button, button, button', and
the seek bar as a bare number.

Every icon-only button now carries a label, the sliders carry a label plus
live value text so the seek position is spoken as a time rather than a
percentage, and the visualizer and album-art drawing areas — which have no
implicit name at all — carry both a label and a description.

Labels name the control, not its shortcut: a screen reader reading
'Playlist (p)' would be reading punctuation aloud."
```

---

## Task 11: List and table semantics (item 1, part 2)

Track rows currently announce as raw concatenated cell text. This task gives the four `ColumnView`s real row semantics, and takes the `TreeView`-backed playlist as far as its deprecated accessible implementation allows.

**Files:**
- Modify: `frontends/gtk/window/files.rs:257`, `playlists.rs:443`, `devices_page.rs:476`, `disc_data.rs:95`
- Modify: `frontends/gtk/window/playlist_window.rs:272`
- Test: `mod tests` in `tests.rs`

**Interfaces:**
- Consumes: Task 10's labelling conventions.
- Produces: nothing other tasks depend on.

- [ ] **Step 1: Name the four column views**

Each `ColumnView` gets a name describing its content, so a screen reader announces which table focus entered:

```rust
        col_view.update_property(&[gtk4::accessible::Property::Label("Tracks")]);
```

Use "Tracks" in `files.rs`, "Playlist tracks" in `playlists.rs`, "Device files" in `devices_page.rs`, and "Disc tracks" in `disc_data.rs`.

- [ ] **Step 2: Give rows a spoken summary**

A row built from a `SignalListItemFactory` announces the concatenation of its cells. Set an explicit row label in the factory's `bind` handler so the announcement is a sentence rather than a run-on.

In `files.rs`, inside the existing `bind` closure where the row's track is already in hand:

```rust
                // Without this a screen reader reads every cell in sequence,
                // including the empty ones. One sentence per row is what the
                // HIG's list guidance asks for.
                let spoken = match (track.artist.is_empty(), track.album.is_empty()) {
                    (false, false) => format!("{}, {}, {}", track.title, track.artist, track.album),
                    (false, true) => format!("{}, {}", track.title, track.artist),
                    _ => track.title.clone(),
                };
                li.update_property(&[gtk4::accessible::Property::Label(&gtk_safe(&spoken))]);
```

Apply the same shape in the other three views, using each one's own row type and the fields it actually displays.

- [ ] **Step 3: Take the playlist TreeView as far as it goes**

`GtkTreeView` is deprecated since GTK 4.10 and does not plumb cell-level names. It can still carry a widget-level name, which is better than nothing:

```rust
    // GtkTreeView (deprecated since 4.10) has no cell-level accessible
    // plumbing, so this widget-level name is the ceiling here. Full row
    // semantics need the ColumnView migration tracked as audit item 11.
    pl_view.update_property(&[gtk4::accessible::Property::Label("Playlist")]);
```

- [ ] **Step 4: Write the regression test**

In `tests.rs`:

```rust
    /// The spoken row summary drops empty fields rather than reading commas
    /// around nothing — an untagged file otherwise announces as
    /// "song, , ".
    #[test]
    fn row_summary_omits_empty_fields() {
        assert_eq!(super::files::spoken_row_summary("Song", "", ""), "Song");
        assert_eq!(super::files::spoken_row_summary("Song", "Artist", ""), "Song, Artist");
        assert_eq!(
            super::files::spoken_row_summary("Song", "Artist", "Album"),
            "Song, Artist, Album"
        );
    }
```

This requires lifting Step 2's `match` into a small free function in `files.rs` so it is testable without GTK:

```rust
/// One-sentence spoken summary of a track row, skipping fields the file
/// does not have. Kept separate from the bind closure so the formatting is
/// unit-testable without constructing a widget.
pub(super) fn spoken_row_summary(title: &str, artist: &str, album: &str) -> String {
    match (artist.is_empty(), album.is_empty()) {
        (false, false) => format!("{title}, {artist}, {album}"),
        (false, true) => format!("{title}, {artist}"),
        _ => title.to_string(),
    }
}
```

Call it from the bind closure in place of the inline `match`.

- [ ] **Step 5: Run the tests**

Run: `distrobox enter dev-box -- sh -c 'cargo test --bin sparkamp row_summary 2>&1 | tail -20'`
Expected: PASS.

- [ ] **Step 6: Run the full suite**

Run: `distrobox enter dev-box -- sh -c 'cargo build && cargo test 2>&1 | tail -20'`
Expected: all pass, zero warnings.

- [ ] **Step 7: Commit**

```bash
git add frontends/gtk/window/
git commit -m "feat(gtk): announce track rows as sentences, not cell runs

A ColumnView row announced the concatenation of every cell, empty ones
included, so an untagged file read as 'song, , '. Each of the four column
views now names itself and gives its rows a one-sentence summary that
skips absent fields.

The active playlist gets a widget-level name only. GtkTreeView is
deprecated since GTK 4.10 and has no cell-level accessible plumbing, so
that is the ceiling until the ColumnView migration — audit item 11, held
back deliberately because the playlist's multi-select drag-reorder
deserves its own branch and its own tests.

The row formatting lives in a free function so it is testable without
constructing a widget."
```

---

## Task 12: Close out the branch

**Files:**
- Modify: `docs/mac-pass-checklist.md`
- Modify: `README.md` (shortcut table, if it documents Ctrl+.)

**Interfaces:**
- Consumes: everything above.
- Produces: the record a later macOS session works from.

- [ ] **Step 1: Log the macOS parity gaps**

Append a section to `docs/mac-pass-checklist.md`:

```markdown
## 2026-08-24 — GTK HIG improvements (branch `gtk-hig-improvements`)

This branch was scoped GTK/Linux only. Each item below is the macOS work
that would bring the frontends back level. None of it was attempted blind.

- [ ] **Accessible names** — no `accessibilityLabel` sweep was done on the
  SwiftUI frontend. GTK now labels every icon-only control, both sliders,
  the drawing areas, and all four column views.
- [ ] **Font scaling** — GTK renders skin font sizes as `pt` so they follow
  the desktop text size, and the built-in defaults re-baselined to native
  11pt (15/18/40 px). macOS reads the same skin variables; whether it
  honours Dynamic Type is unverified, and its built-ins will now differ
  from GTK's unless the same re-baseline is applied.
- [ ] **Shortcuts** — Cmd+, already matches GTK's new Ctrl+,. F1 and Cmd+F
  aliases are absent on macOS.
- [ ] **Track-change notification** — GTK posts one when unfocused, with a
  Settings toggle (`playback.notify_track_change`). macOS has no
  equivalent; `UNUserNotificationCenter` is the natural fit.
- [ ] **Toasts** — GTK demotes recoverable failures to `AdwToastOverlay`.
  macOS still reports them as alerts.
- [ ] **Empty states** — GTK has placeholder pages on six views.
  `ContentUnavailableView` is the macOS equivalent.

Not applicable to macOS: the packaging metadata work, the TUI `/` fix, and
mnemonics (macOS menus carry their own key equivalents).
```

- [ ] **Step 2: Update the README shortcut table**

Check whether `README.md` documents Ctrl+. and update it to Ctrl+,, and add the new aliases:

```bash
grep -n "Ctrl+\.\|Ctrl+F\|F1" README.md
```

Fix any hit that names a binding this branch changed.

- [ ] **Step 3: Verify the whole branch builds clean**

Run: `distrobox enter dev-box -- sh -c 'cargo build && cargo test 2>&1 | tail -25'`
Expected: all pass, zero warnings.

- [ ] **Step 4: Commit**

```bash
git add docs/mac-pass-checklist.md README.md
git commit -m "docs: record the macOS gaps this branch opens

The branch was scoped GTK/Linux only, so each item is logged with what the
macOS equivalent would be — a shopping list for a session on a Mac, rather
than something rediscovered later.

The font re-baseline is the one that matters most: macOS reads the same
skin variables, so its built-ins now differ from GTK's until the same
change is applied there."
```

- [ ] **Step 5: Hand back for the interactive pass**

The branch is code-complete but not verified on screen. Report to the user that these need eyes:

1. **Adw stylesheet drift** — the toast pill and the six `AdwStatusPage`s inside skinned windows. This is the judgment call the design flagged; the fallback for either is a hand-built skin-styled widget behind the same helper signature.
2. **The re-baselined text** at both default and large-text settings. The player is `resizable(false)` with no hard max-width, so it should grow rather than clip — confirm it does.
3. **Screenshots** — five PNGs into `docs/screenshots/`, per that directory's README. The metainfo already references them by name.
4. **The notification** — play a track with another window focused, confirm one banner appears and that it does not appear while the player is focused.
5. **Orca**, if available, over the transport row and a track list.

---

## Notes for the implementer

- **Read before editing.** `CLAUDE.md` is explicit: verify the current state of the code rather than trusting a summary, including this plan. Line numbers here were accurate when it was written and drift as tasks land.
- **Two agent failures on the same problem means stop and ask.** Do not loop.
- **Ask before refactoring.** These tasks touch large files; resist the urge to tidy what you are passing through.
- **The `~/Music` write trap:** that path is a symlink to a volume the distrobox mounts read-only. A playlist save or tag write that appears to silently do nothing is usually this, not a bug. Check with `distrobox enter dev-box -- touch ~/Music/.probe` before debugging any such report.
