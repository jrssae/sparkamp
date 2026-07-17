//! Skin loading — built-in CSS skins and user-provided overrides.
// Public API for skin discovery and loading; some functions are for future use.
#![allow(dead_code)]
//!
//! ## Skin discovery
//!
//! Skins are identified by their **stem name** (lower-case, no path, no
//! extension).  Sparkamp looks for a matching skin in this order:
//!
//! 1. **User-provided** — a `.css` file in `~/.config/sparkamp/skins/` whose
//!    stem equals the requested name.  If found, it takes priority over the
//!    built-in skin with the same name.
//! 2. **Built-in** — `"dark"` or `"light"`, embedded in the binary at compile
//!    time from `frontends/gtk/style_dark.css` and `frontends/gtk/style_light.css`.
//!
//! ## Accent-colour injection
//!
//! Both built-in skins use `@accent_bg_color` and `@accent_fg_color` CSS
//! variables.  Before a skin is passed to GTK4, these are resolved via
//! `@define-color` declarations prepended to the CSS text.  The accent colour
//! is read from the GNOME `accent-color` gsettings key at startup and falls
//! back to GNOME's default blue (`#3584e4`) when unavailable.
//!
//! User-provided skin files may also use `@accent_bg_color` / `@accent_fg_color`
//! and will benefit from the same injection.
//!
//! ## CSS class reference
//!
//! See `frontends/gtk/style_dark.css` for a full annotated list of every CSS
//! class name used by Sparkamp.  User skins that target the same class names
//! will override the default values.  The canonical list:
//!
//! | Class / selector                   | Widget                                          |
//! |------------------------------------|------------------------------------------------|
//! | `window`                           | Main application window background              |
//! | `.np-title`                        | Now-playing track title label                   |
//! | `.np-artist`                       | Now-playing artist label                        |
//! | `.np-frame`                        | Border frame around the marquee / title area    |
//! | `.time-disp`                       | Large digital time counter                      |
//! | `button.transport`                 | z / x / c / v / b transport buttons             |
//! | `button.transport-play`            | Play button (x) — accent-coloured               |
//! | `scale.seek-scale`                 | Seek / scrub bar                                |
//! | `scale.vol-scale`                  | Volume slider                                   |
//! | `.vol-label`                       | Volume percentage label                         |
//! | `.mini-viz`                        | Mini visualizer DrawingArea border              |
//! | `button.mode-btn`                  | Repeat / Shuffle / PL / Info mode buttons       |
//! | `button.mode-btn-active`           | Mode button when its feature is enabled          |
//! | `.playlist`                        | Playlist ListBox                                |
//! | `.playlist row`                    | Individual playlist row                         |
//! | `.playlist row.playing`            | The currently-playing row                       |
//! | `.playlist row.broken`             | A row whose file is missing / unplayable        |
//! | `.playlist row.dragging`           | A row being dragged for reorder                 |
//! | `.playlist row.drop-target`        | The row below the current drag drop point       |
//! | `.pl-dur-label`                    | Per-track duration label (right column)         |
//! | `.pl-count-label`                  | Playlist total-count label                      |
//! | `button.pl-btn`                    | Playlist Add / Remove / Clear buttons           |
//! | `button.pl-btn.destructive`        | Clear-all / Remove button (red tint)            |
//! | `.status-label`                    | One-line status bar at the bottom of the window |
//! | `.info-text`                       | Monospace body text in the Info window          |

use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Built-in skin templates (embedded at compile time)
// ---------------------------------------------------------------------------

/// The built-in Dark skin template (also what Download skin… exports for Dark).
pub const DARK_TEMPLATE_CSS: &str = include_str!("skin_templates/dark.css");

/// The built-in Light skin template.
pub const LIGHT_TEMPLATE_CSS: &str = include_str!("skin_templates/light.css");

/// The bundled skin how-to guide (Markdown).
pub const SKIN_GUIDE_MD: &str = include_str!("skin_templates/skin-guide.md");

// ---------------------------------------------------------------------------
// Rgb color type
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// SkinVars struct
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Skin struct
// ---------------------------------------------------------------------------

/// A resolved skin: name, parsed vars, and origin.
#[derive(Debug, Clone)]
pub struct Skin {
    /// Lower-case stem name (e.g. `"dark"`, `"light"`, `"my-theme"`).
    pub name: String,

    /// Parsed 14-variable theme data.
    pub vars: SkinVars,

    /// Where this skin was loaded from.
    pub source: SkinSource,
}

/// Origin of a loaded skin.
#[derive(Debug, Clone)]
pub enum SkinSource {
    /// One of the skins compiled into the binary.
    BuiltIn,
    /// A `.css` file from the user's skins directory.
    UserFile(PathBuf),
}

// ---------------------------------------------------------------------------
// SkinEntry — lightweight listing entry for the Appearance pane
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// --sp-* skin variable parser
// ---------------------------------------------------------------------------

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

fn strip_css_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut chars = css.char_indices().peekable();
    while let Some((_, ch)) = chars.next() {
        if ch == '/' {
            if chars.peek().map(|(_, c)| *c) == Some('*') {
                chars.next(); // consume '*'
                // scan forward for */
                loop {
                    match chars.next() {
                        Some((_, '*')) if chars.peek().map(|(_, c)| *c) == Some('/') => {
                            chars.next(); // consume '/'
                            break;
                        }
                        None => break,
                        _ => {}
                    }
                }
                continue;
            }
        }
        out.push(ch);
    }
    out
}

// ---------------------------------------------------------------------------
// Skin directory helpers
// ---------------------------------------------------------------------------

/// Return the path to the user's skins directory:
/// `$XDG_CONFIG_HOME/sparkamp/skins/` (defaults to
/// `~/.config/sparkamp/skins/` on Linux).
pub fn user_skins_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("sparkamp")
        .join("skins")
}

/// Enumerate all skin names available to the user.
///
/// Returns a list of stem names (lower-case, no extension).  Built-in skins
/// come first (`"dark"`, `"light"`), followed by any `.css` files found in
/// the user's skins directory, sorted alphabetically.  Duplicate names are
/// deduplicated (user files shadow built-ins).
pub fn available_skins() -> Vec<String> {
    list_skins_in(&user_skins_dir(), &[])
        .into_iter()
        .map(|e| e.name)
        .collect()
}

/// Load a skin by stem name, searching user files first then built-ins.
///
/// Returns `None` if no skin with that name is found.
pub fn load_skin(name: &str) -> Option<Skin> {
    let lower = name.to_lowercase();

    // User file wins.
    let user_path = user_skins_dir().join(format!("{lower}.css"));
    if let Ok(css) = std::fs::read_to_string(&user_path) {
        let vars = parse_skin_vars(&css);
        return Some(Skin {
            name:   lower,
            vars,
            source: SkinSource::UserFile(user_path),
        });
    }

    // Built-ins.
    match lower.as_str() {
        "dark" => Some(Skin {
            name:   lower,
            vars:   SkinVars::dark_defaults(),
            source: SkinSource::BuiltIn,
        }),
        "light" => Some(Skin {
            name:   lower,
            vars:   SkinVars::light_defaults(),
            source: SkinSource::BuiltIn,
        }),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Add user skin (Appearance → Add skin…)
// ---------------------------------------------------------------------------

/// Error from [`add_user_skin`] / [`add_user_skin_to`].
#[derive(Debug)]
pub enum SkinError {
    ReadFailed(std::io::Error),
    WriteFailed(std::io::Error),
    NoRootBlock,
}

impl std::fmt::Display for SkinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkinError::ReadFailed(e)  => write!(f, "could not read skin file: {e}"),
            SkinError::WriteFailed(e) => write!(f, "could not write skin file: {e}"),
            SkinError::NoRootBlock    => write!(f,
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
    // Fallback — extremely unlikely; use a timestamp suffix.
    let s = format!("{stem}-{}", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0));
    let p = dir.join(format!("{s}.css"));
    (s, p)
}

// ---------------------------------------------------------------------------
// Skin listing (Appearance pane)
// ---------------------------------------------------------------------------

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
            .filter(|p| p.extension().is_some_and(|ext| ext == "css"))
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

/// Render a complete GTK4 stylesheet from the given skin vars.
///
/// The output includes every widget class used by Sparkamp's GTK frontend.
/// Derivations (row backgrounds, dim text, borders) are inlined from
/// the vars at emit time.
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
        color: {hl}; font-size: {fsm}px; font-weight: bold; padding: 2px 0px; \
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
        padding: 2px 4px; min-width: 24px; min-height: 24px; box-shadow: none; \
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
        border: 1px solid {border}; border-radius: 3px; \
        padding: 2px 4px; min-width: 28px; \
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

    // Seek bar — slim trough, chunky rectangular handle that overflows ±5px.
    writeln!(css, "scale.seek-scale trough {{ \
        background-color: {tbg}; background-image: none; \
        min-height: 4px; \
    }}").unwrap();
    writeln!(css, "scale.seek-scale highlight {{ \
        background-color: {hl}; background-image: none; \
    }}").unwrap();
    writeln!(css, "scale.seek-scale slider {{ \
        background-color: {hl}; background-image: none; \
        border-radius: 3px; margin: -5px; min-width: 18px; min-height: 18px; \
    }}").unwrap();

    // Volume slider — same chunky overflow style as seek.
    writeln!(css, "scale.vol-scale trough {{ \
        background-color: {tbg}; background-image: none; \
        min-height: 4px; \
    }}").unwrap();
    writeln!(css, "scale.vol-scale highlight {{ \
        background-color: {hl}; background-image: none; \
    }}").unwrap();
    writeln!(css, "scale.vol-scale slider {{ \
        background-color: {hl}; background-image: none; \
        border-radius: 3px; margin: -5px; min-width: 18px; min-height: 18px; \
    }}").unwrap();
    writeln!(css, ".vol-label {{ \
        color: {text_dim}; font-size: {fs}px; font-family: monospace; min-width: 28px; \
    }}").unwrap();

    // Mini visualizer — no inner border so the time-display row above it and
    // the visualizer below it read as one continuous LCD column (matches the
    // macOS layout, where the left column is a single dark box).
    writeln!(css, ".mini-viz {{ \
        background-color: {tbg}; border: none; \
    }}").unwrap();

    // Equalizer window scales (horizontal pre-amp + vertical band columns).
    // Same chunky overflow handle as seek/vol; trough slimmed on both axes
    // so vertical band sliders read as thin columns and the horizontal
    // preamp matches the main-window seek bar.
    writeln!(css, "scale.eq-scale trough {{ \
        background-color: {tbg}; background-image: none; \
        min-width: 4px; min-height: 4px; \
    }}").unwrap();
    writeln!(css, "scale.eq-scale highlight {{ \
        background-color: {hl}; background-image: none; \
    }}").unwrap();
    writeln!(css, "scale.eq-scale slider {{ \
        background-color: {hl}; background-image: none; \
        border-radius: 3px; margin: -5px; min-width: 18px; min-height: 18px; \
    }}").unwrap();
    writeln!(css, "scale.eq-scale label {{ \
        color: {text_dim}; font-size: {fs}px; \
    }}").unwrap();

    // Playlist + Media Library list/columnview.
    // `.ml-sidebar` is the left-nav ListBox in the Media Library window;
    // `.rich-list` is the skin selector in Settings → Appearance. Both
    // wrap their own ScrolledWindow, which would otherwise render with
    // the system default (often dark) background.
    writeln!(css, ".playlist, .ml-sidebar, .rich-list, \
                   columnview, listview, list {{ \
        background-color: {tbg}; color: {text}; font-size: {fs}px; \
    }}").unwrap();
    writeln!(css, ".ml-sidebar row, .rich-list row {{ \
        color: {text}; padding: 2px 4px; \
    }}").unwrap();
    // ScrolledWindow wrapping these lists — match so the corners and
    // scrollbar gutter don't bleed the system theme through.
    writeln!(css, "scrolledwindow > viewport > .ml-sidebar, \
                   scrolledwindow > viewport > .rich-list, \
                   scrolledwindow > viewport > .playlist {{ \
        background-color: {tbg}; \
    }}").unwrap();
    writeln!(css, ".playlist row, columnview row, listview row, \
                   .ml-col-view row {{ \
        color: {text}; \
    }}").unwrap();
    writeln!(css, ".playlist row:hover, .ml-sidebar row:hover, \
                   columnview row:hover, listview row:hover, \
                   .ml-col-view row:hover {{ \
        background: {hl_hov}; \
    }}").unwrap();
    // `.ml-col-view` is on the hand-built GtkListBoxes (the burn-panel queue
    // and the audio-CD track list) — a listbox, not a columnview/listview/
    // treeview, so it needs its own selector here or its selected rows fall
    // back to GTK's default (wrong) accent colour. Harmless on the real
    // ColumnViews that also carry the class (already covered above).
    writeln!(css, ".playlist row:selected, .ml-sidebar row:selected, \
                   columnview row:selected, listview row:selected, \
                   .ml-col-view row:selected {{ \
        background: {hl_sel}; color: {text}; \
    }}").unwrap();
    // GtkTreeView paints rows as a single widget with :selected state
    // rather than per-row sub-widgets like ListBox/ColumnView.  The
    // `.playlist row:selected` rule above misses it, so add treeview-
    // specific selectors covering the GTK4 node hierarchy
    // (`treeview.view.playlist`).  Without these the active playlist's
    // selected row is invisible against the skin background.
    writeln!(css, ".playlist:selected, \
                   .playlist:selected:focus, \
                   .playlist:selected:hover, \
                   treeview.view.playlist:selected, \
                   treeview.view.playlist:selected:focus, \
                   treeview.view.playlist:selected:hover, \
                   treeview.playlist:selected, \
                   treeview.playlist:selected:focus {{ \
        background-color: {hl_sel}; color: {text}; \
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
                   columnview row.broken label, columnview row.broken cell, \
                   label.broken {{ \
        color: {broken}; \
    }}").unwrap();
    writeln!(css, ".pl-dur-label {{ color: {text_dim}; font-family: monospace; }}").unwrap();
    writeln!(css, ".pl-count-label {{ color: {text}; font-size: {fs}px; }}").unwrap();
    // Artwork "View" button inside ColumnView cells: strip the default button
    // min-height/padding so an art row is exactly as tall as a text row, and
    // every row in the files / device track view has a uniform height.
    writeln!(css, "columnview cell button {{ \
        min-height: 0; padding: 0 4px; margin: 0; \
    }}").unwrap();

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
    // Device overview cards (the Devices page list).
    writeln!(css, ".device-card {{ \
        background-color: {tbg}; border: 1px solid {border}; border-radius: 8px; \
        padding: 10px 12px; \
    }}").unwrap();
    // Burn progress overlay card — SOLID background so the phase text/bar is
    // readable over the detail view (the GTK `osd` style is translucent).
    writeln!(css, ".burn-overlay-card {{ \
        background-color: {bg}; border: 1px solid {border}; border-radius: 10px; \
        padding: 16px 18px; \
    }}").unwrap();
    writeln!(css, ".device-card-name {{ \
        color: {text}; font-size: {fs}px; font-weight: bold; \
    }}").unwrap();
    writeln!(css, ".device-badge {{ \
        color: {text_dim}; border: 1px solid {border}; border-radius: 999px; \
        padding: 1px 8px; font-size: {fs}px; \
    }}").unwrap();
    writeln!(css, ".device-badge-warn {{ color: {broken}; border-color: {broken}; }}").unwrap();
    // Smaller badge variant (2pt smaller font + tighter padding).
    writeln!(css, ".device-badge-sm {{ font-size: {}px; padding: 0px 6px; }}", fs - 2.0).unwrap();
    // Device detail page: header band, storage section, bottom status bar.
    writeln!(css, ".device-detail-header {{ \
        background-color: {tbg}; border: 1px solid {border}; border-radius: 8px; \
        padding: 10px 12px; \
    }}").unwrap();
    writeln!(css, ".device-detail-name {{ \
        color: {text}; font-size: {fs}px; font-weight: bold; \
    }}").unwrap();
    writeln!(css, ".device-section {{ padding: 4px 2px; }}").unwrap();
    writeln!(css, ".device-statusbar {{ \
        border-top: 1px solid {border}; padding: 4px 2px; \
    }}").unwrap();
    // Storage capacity meter + copy-progress bar: same chunky height + rounded
    // accent fill so the two read as a matched pair on the detail page.
    writeln!(css, "levelbar.device-capacity trough, \
                   levelbar.device-capacity trough block {{ \
        min-height: 12px; border-radius: 6px; \
    }}").unwrap();
    writeln!(css, "levelbar.device-capacity trough {{ background-color: {tbg}; }}").unwrap();
    writeln!(css, "levelbar.device-capacity trough block.filled {{ background-color: {hl}; }}").unwrap();
    writeln!(css, "progressbar.device-progress trough, \
                   progressbar.device-progress progress {{ \
        min-height: 12px; border-radius: 6px; \
    }}").unwrap();
    writeln!(css, "progressbar.device-progress trough {{ background-color: {tbg}; }}").unwrap();
    writeln!(css, "progressbar.device-progress progress {{ background-color: {hl}; }}").unwrap();
    // Capacity LevelBar fullness colors (filled portion). One class is applied
    // per bar by set_levelbar_fullness, so every capacity bar (sidebar row,
    // overview card, detail header) is colored identically regardless of whether
    // it also carries the device-capacity sizing class: blue/accent when safe,
    // amber under 15% free, red under 5% free.
    writeln!(css, "levelbar.cap-ok trough block.filled {{ background-color: {hl}; }}").unwrap();
    writeln!(css, "levelbar.cap-warn trough block.filled {{ background-color: #d08a16; }}").unwrap();
    writeln!(css, "levelbar.cap-full trough block.filled {{ background-color: {broken}; }}").unwrap();
    // Device playlist filter chips (grouped toggle buttons).
    writeln!(css, ".device-chips {{ padding: 2px 0; }}").unwrap();
    writeln!(css, "button.device-chip {{ \
        background-color: {btn}; background-image: none; color: {btext}; \
        border: 1px solid {border}; border-radius: 999px; padding: 2px 12px; \
        min-height: 0; \
    }}").unwrap();
    // Checked chip mirrors the main player's active mode button (button_active
    // fill + button text) so a selected playlist reads with the same highlight
    // as a selected button elsewhere, with an accent border to mark it active.
    writeln!(css, "button.device-chip:checked {{ \
        background-color: {bact}; color: {btext}; border-color: {hl}; \
    }}").unwrap();
    writeln!(css, ".info-text {{ \
        color: {text}; background-color: {tbg}; font-family: {ff}; font-size: {fs}px; \
        padding: 6px; border-radius: 3px; \
    }}").unwrap();
    writeln!(css, ".info-title {{ \
        color: {text}; font-family: {ff}; font-size: {fs}px; font-weight: bold; \
        margin-bottom: 4px; \
    }}").unwrap();
    writeln!(css, ".info-section {{ \
        color: {hl}; font-family: {ff}; font-size: {fs}px; font-weight: bold; \
        margin-top: 6px; \
    }}").unwrap();
    writeln!(css, ".info-key {{ \
        color: {text}; font-family: {ff}; font-size: {fs}px; font-weight: bold; \
        padding-left: 8px; \
    }}").unwrap();
    writeln!(css, ".info-desc {{ \
        color: {text}; font-family: {ff}; font-size: {fs}px; \
    }}").unwrap();
    // About pane
    writeln!(css, ".about-title {{ \
        color: {text}; font-family: {ff}; font-size: {fsl}px; font-weight: bold; \
    }}", fsl = v.font_size_large).unwrap();
    writeln!(css, ".about-section {{ \
        color: {hl}; font-family: {ff}; font-size: {fs}px; font-weight: bold; \
    }}").unwrap();
    writeln!(css, ".about-subtle {{ \
        color: {text_dim}; font-family: {ff}; font-size: {fs}px; \
    }}").unwrap();

    // Form inputs sitting on text-background
    writeln!(css, "entry, textview {{ \
        background-color: {tbg}; color: {text}; caret-color: {hl}; \
        border: 1px solid {border}; \
    }}").unwrap();
    writeln!(css, "entry:focus, entry:focus-within, textview:focus, textview:focus-within {{ \
        border-color: {hl}; outline: 1px solid {hl}; outline-offset: -1px; \
    }}").unwrap();

    // Notebook (settings window tabs)
    writeln!(css, "notebook {{ \
        background-color: {bg}; color: {text}; \
    }}").unwrap();
    writeln!(css, "notebook > header {{ \
        background-color: {bg}; border-color: {border}; \
    }}").unwrap();
    writeln!(css, "notebook > header > tabs > tab {{ \
        background-color: {btn}; color: {btext}; \
        border: 1px solid {border}; padding: 4px 10px; \
    }}").unwrap();
    writeln!(css, "notebook > header > tabs > tab:hover {{ \
        background-color: {bhov}; \
    }}").unwrap();
    writeln!(css, "notebook > header > tabs > tab:checked {{ \
        background-color: {bact}; color: {btext}; \
        border-color: {hl}; \
        box-shadow: inset 0 -2px 0 {hl}; \
    }}").unwrap();
    writeln!(css, "notebook > stack {{ \
        background-color: {bg}; color: {text}; \
    }}").unwrap();

    // Checkboxes + radios: active color when checked
    writeln!(css, "checkbutton check, checkbutton radio {{ \
        background-color: {tbg}; background-image: none; \
        border: 1px solid {border}; \
    }}").unwrap();
    writeln!(css, "checkbutton check:checked, checkbutton radio:checked {{ \
        background-color: {bact}; background-image: none; \
        border-color: {hl}; color: {btext}; \
        -gtk-icon-filter: none; \
    }}").unwrap();

    // Title bar buttons: reset our generic button styling so the window
    // controls fall back to the system default size/shape.
    writeln!(css, "windowcontrols button, headerbar button.titlebutton {{ \
        padding: 0; min-width: 0; min-height: 0; \
        background-color: transparent; background-image: none; \
        border: none; box-shadow: none; \
    }}").unwrap();
    writeln!(css, "windowcontrols button:hover, headerbar button.titlebutton:hover {{ \
        background-color: {bhov}; \
    }}").unwrap();

    // ── Popover-menu styling for the plain-Popover playlist-editor
    //    right-click menu — make Buttons inside `.menu` look like the
    //    GtkModelButton entries that PopoverMenu renders natively.
    writeln!(css, "popover.menu contents {{ \
        background-color: {bg}; padding: 4px 0; \
    }}").unwrap();
    writeln!(css, "popover.menu box.menu button.modelbutton {{ \
        background-color: transparent; background-image: none; \
        border: none; border-radius: 0; box-shadow: none; \
        padding: 6px 14px; min-height: 0; \
        color: {text}; \
    }}").unwrap();
    writeln!(css, "popover.menu box.menu button.modelbutton:hover {{ \
        background-color: {bhov}; \
    }}").unwrap();
    writeln!(css, "popover.menu box.menu button.modelbutton label {{ \
        font-weight: normal; \
    }}").unwrap();
    writeln!(css, "popover.menu box.menu separator {{ \
        background-color: {border}; min-height: 1px; \
        margin: 4px 8px; \
    }}").unwrap();
    writeln!(css, "popover.menu box.menu label.dim-label {{ \
        color: {btext}; padding: 4px 14px 2px 14px; \
        font-size: 0.85em; \
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Loading the "dark" skin by name must succeed and return built-in source.
    #[test]
    fn load_skin_dark_returns_builtin() {
        let skin = load_skin("dark").expect("dark skin must be available");
        assert_eq!(skin.name, "dark");
        assert!(matches!(skin.source, SkinSource::BuiltIn));
    }

    /// Loading the "light" skin by name must succeed and return built-in source.
    #[test]
    fn load_skin_light_returns_builtin() {
        let skin = load_skin("light").expect("light skin must be available");
        assert_eq!(skin.name, "light");
        assert!(matches!(skin.source, SkinSource::BuiltIn));
    }

    /// Skin name lookup is case-insensitive.
    #[test]
    fn load_skin_is_case_insensitive() {
        assert!(load_skin("Dark").is_some());
        assert!(load_skin("LIGHT").is_some());
        assert!(load_skin("Light").is_some());
    }

    /// An unknown skin name returns None.
    #[test]
    fn load_skin_unknown_returns_none() {
        assert!(load_skin("nonexistent_skin_xyz_123").is_none());
    }

    /// `available_skins` always includes the two built-in skins.
    #[test]
    fn available_skins_always_includes_builtins() {
        let skins = available_skins();
        assert!(skins.contains(&"dark".to_string()));
        assert!(skins.contains(&"light".to_string()));
    }

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
    fn load_skin_is_case_insensitive_new() {
        assert!(load_skin("Dark").is_some());
        assert!(load_skin("LIGHT").is_some());
    }

    #[test]
    fn load_skin_unknown_returns_none_new() {
        assert!(load_skin("nonexistent_skin_xyz_abc").is_none());
    }

    #[test]
    fn list_skins_no_user_returns_only_builtins() {
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

    // -----------------------------------------------------------------------
    // render_gtk_css
    // -----------------------------------------------------------------------

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
}
