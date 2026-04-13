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
// Built-in skin CSS (embedded at compile time)
// ---------------------------------------------------------------------------

/// Raw CSS for the built-in dark skin.  Accent colours are resolved at
/// runtime by [`prepare_css`] before the CSS is loaded into GTK.
pub const DARK_CSS_RAW: &str = include_str!("../frontends/gtk/style_dark.css");

/// Raw CSS for the built-in light skin.
pub const LIGHT_CSS_RAW: &str = include_str!("../frontends/gtk/style_light.css");

/// All built-in skins as `(name, raw_css)` pairs.
pub const BUILTIN_SKINS: &[(&str, &str)] = &[
    ("dark",  DARK_CSS_RAW),
    ("light", LIGHT_CSS_RAW),
];

// ---------------------------------------------------------------------------
// Skin struct
// ---------------------------------------------------------------------------

/// A resolved skin: name, raw CSS, and origin.
#[derive(Debug, Clone)]
pub struct Skin {
    /// Lower-case stem name (e.g. `"dark"`, `"light"`, `"my-theme"`).
    pub name: String,

    /// Raw CSS text, including any `@define-color` or `@import` declarations.
    /// Accent-colour variables are **not** yet injected — call [`prepare_css`].
    pub css_raw: String,

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
// Accent-colour injection
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Accent-colour injection + --sparkamp-* variable support
// ---------------------------------------------------------------------------

/// Prepare a CSS string for use with GTK4.
///
/// This does two things:
///
/// 1. **Accent injection** — prepends `@define-color` declarations for
///    `accent_bg_color` / `accent_fg_color` so that skins using those tokens
///    work regardless of the active GTK theme.
///
/// 2. **`--sparkamp-*` translation** — if the skin defines any
///    `--sparkamp-*` custom properties in a `:root` block, those are parsed
///    and converted into concrete GTK CSS selector rules appended at the end.
///    This lets the same skin file work on GTK4 (which does not render
///    `var(--sparkamp-*)` on its widget tree) *and* on macOS (which parses
///    the variables directly).
///
/// `accent_hex` should be a `"#rrggbb"` hex colour string such as `"#3584e4"`.
pub fn prepare_css(raw: &str, accent_hex: &str) -> String {
    let mut out = format!(
        "@define-color accent_bg_color {accent_hex};\n\
         @define-color accent_fg_color #ffffff;\n\
         {raw}\n"
    );
    // If the skin uses --sparkamp-* variables, translate them into GTK rules.
    let vars = parse_sparkamp_vars(raw);
    if !vars.is_empty() {
        out.push_str("/* --sparkamp-* overrides generated by Sparkamp skin loader */\n");
        out.push_str(&generate_gtk_overrides(&vars));
    }
    out
}

/// Detect whether a skin CSS string represents a dark or light theme by
/// reading the luminance of `--sparkamp-background`.
///
/// Returns `true` for dark (luminance < 0.5) and `false` for light.
/// Falls back to `true` (dark) when the variable is absent or unparseable.
pub fn is_dark_skin(raw: &str) -> bool {
    let vars = parse_sparkamp_vars(raw);
    vars.get("--sparkamp-background")
        .and_then(|hex| hex_luminance(hex.trim()))
        .map(|l| l < 0.5)
        .unwrap_or(true)
}

// ---------------------------------------------------------------------------
// --sparkamp-* variable parser
// ---------------------------------------------------------------------------

/// Extract all `--sparkamp-*` custom properties defined inside a `:root { … }`
/// block in the given CSS text.  Returns a map of variable name → value.
///
/// Only the *first* `:root` block is scanned.  Comments are stripped before
/// parsing.  Values are trimmed of whitespace and trailing semicolons.
pub fn parse_sparkamp_vars(css: &str) -> std::collections::HashMap<String, String> {
    let stripped = strip_css_comments(css);
    let mut map = std::collections::HashMap::new();

    // Find :root { … }
    let Some(open_pos) = find_root_open(&stripped) else {
        return map;
    };
    let block_start = open_pos + 1;
    let Some(close_pos) = stripped[block_start..].find('}') else {
        return map;
    };
    let block = &stripped[block_start..block_start + close_pos];

    for stmt in block.split(';') {
        let stmt = stmt.trim();
        if !stmt.starts_with("--sparkamp-") {
            continue;
        }
        let Some(colon) = stmt.find(':') else { continue };
        let key = stmt[..colon].trim().to_string();
        let val = stmt[colon + 1..].trim().to_string();
        if !key.is_empty() && !val.is_empty() {
            map.insert(key, val);
        }
    }
    map
}

fn find_root_open(css: &str) -> Option<usize> {
    // Simple scan: find ":root" followed by optional whitespace then "{"
    let mut search = css;
    let mut offset = 0;
    while let Some(pos) = search.find(":root") {
        let after = search[pos + 5..].trim_start();
        if after.starts_with('{') {
            let brace_off = search[pos + 5..].find('{').unwrap();
            return Some(offset + pos + 5 + brace_off);
        }
        offset += pos + 5;
        search = &search[pos + 5..];
    }
    None
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
// --sparkamp-* → GTK CSS override generator
// ---------------------------------------------------------------------------

/// Maps each `--sparkamp-*` variable to one or more GTK CSS rules.
/// Each entry is `(variable_name, gtk_css_template)` where `{v}` is replaced
/// by the variable's value.  Entries with an empty template are skipped
/// (e.g. logo variables that have no GTK equivalent).
static SPARKAMP_TO_GTK: &[(&str, &str)] = &[
    // Window background
    ("--sparkamp-background",
     "window { background-color: {v}; }"),
    // LCD / Now-Playing area
    ("--sparkamp-lcd-background",
     ".np-frame { background-color: {v}; } \
      .mini-viz { background-color: {v}; }"),
    // Title — drives np-title label and the large time display
    ("--sparkamp-title-text",
     ".np-title { color: {v}; } \
      .time-disp { color: {v}; }"),
    // Artist subtitle
    ("--sparkamp-artist-text",
     ".np-artist { color: {v}; }"),
    // Time override (separate from title if the skin wants them different)
    ("--sparkamp-time-text",
     ".time-disp { color: {v}; }"),
    // Transport buttons
    ("--sparkamp-transport-bg",
     "button.transport { background-color: {v}; background-image: none; }"),
    ("--sparkamp-transport-border",
     "button.transport { border-color: {v}; }"),
    ("--sparkamp-transport-text",
     "button.transport { color: {v}; }"),
    ("--sparkamp-transport-hover-bg",
     "button.transport:hover { background-color: {v}; background-image: none; }"),
    ("--sparkamp-transport-active-bg",
     "button.transport:active { background-color: {v}; background-image: none; }"),
    // Play button accent
    ("--sparkamp-play-button-bg",
     "button.transport-play { background-color: {v}; background-image: none; }"),
    ("--sparkamp-play-button-text",
     "button.transport-play { color: {v}; }"),
    ("--sparkamp-play-button-border",
     "button.transport-play { border-color: {v}; }"),
    // Seek bar
    ("--sparkamp-seek-track",
     "scale.seek-scale trough { background-color: {v}; background-image: none; }"),
    ("--sparkamp-seek-fill",
     "scale.seek-scale highlight { background-color: {v}; background-image: none; } \
      scale.seek-scale slider { background-color: {v}; background-image: none; }"),
    ("--sparkamp-seek-thumb",
     "scale.seek-scale slider { background-color: {v}; background-image: none; }"),
    // Volume slider
    ("--sparkamp-volume-track",
     "scale.vol-scale trough { background-color: {v}; background-image: none; }"),
    ("--sparkamp-volume-fill",
     "scale.vol-scale highlight { background-color: {v}; background-image: none; }"),
    ("--sparkamp-volume-thumb",
     "scale.vol-scale slider { background-color: {v}; background-image: none; }"),
    // Playlist
    ("--sparkamp-playlist-bg",
     ".playlist { background-color: {v}; } \
      columnview, listview { background-color: {v}; }"),
    ("--sparkamp-playlist-text",
     ".playlist cell { color: {v}; } \
      .playlist row label { color: {v}; }"),
    ("--sparkamp-playlist-current-text",
     ".playlist row.playing label { color: {v}; } \
      .playlist row.playing cell { color: {v}; }"),
    ("--sparkamp-playlist-current-bg",
     ".playlist row.playing { background-color: {v}; }"),
    ("--sparkamp-playlist-selected-bg",
     ".playlist row:selected { background: {v}; }"),
    ("--sparkamp-playlist-broken-text",
     ".playlist row.broken label { color: {v}; }"),
    ("--sparkamp-playlist-duration-text",
     ".pl-dur-label { color: {v}; }"),
    // Mode buttons (repeat, shuffle, PL toggle)
    ("--sparkamp-mode-btn-bg",
     "button.mode-btn { background-color: {v}; background-image: none; }"),
    ("--sparkamp-mode-btn-border",
     "button.mode-btn { border-color: {v}; }"),
    ("--sparkamp-mode-btn-text",
     "button.mode-btn { color: {v}; }"),
    ("--sparkamp-mode-btn-active-bg",
     "button.mode-btn.mode-btn-active { background-color: {v}; background-image: none; }"),
    ("--sparkamp-mode-btn-active-text",
     "button.mode-btn.mode-btn-active { color: {v}; }"),
    // Logo — GTK uses a raster logo image, not CSS text; these vars are
    // macOS-only and are intentionally left unmapped on GTK.
    ("--sparkamp-logo-text",    ""),
    ("--sparkamp-logo-subtext", ""),
];

/// Generate GTK CSS override rules from a `--sparkamp-*` variable map.
fn generate_gtk_overrides(vars: &std::collections::HashMap<String, String>) -> String {
    let mut out = String::new();
    for (var, template) in SPARKAMP_TO_GTK {
        if template.is_empty() {
            continue;
        }
        if let Some(val) = vars.get(*var) {
            // Each template may contain multiple rules separated by spaces —
            // they're already valid CSS and we just replace {v} with the value.
            out.push_str(&template.replace("{v}", val));
            out.push('\n');
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Luminance helper (for dark/light detection)
// ---------------------------------------------------------------------------

/// Compute the relative luminance of an `#rrggbb` / `#rgb` hex colour.
/// Returns `None` if the string cannot be parsed.
fn hex_luminance(hex: &str) -> Option<f64> {
    let hex = hex.trim_start_matches('#');
    let (r, g, b) = match hex.len() {
        3 => {
            let r = u8::from_str_radix(&hex[0..1].repeat(2), 16).ok()?;
            let g = u8::from_str_radix(&hex[1..2].repeat(2), 16).ok()?;
            let b = u8::from_str_radix(&hex[2..3].repeat(2), 16).ok()?;
            (r, g, b)
        }
        6 | 8 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            (r, g, b)
        }
        _ => return None,
    };
    // sRGB → linear → luminance (ITU-R BT.709)
    let lin = |c: u8| {
        let s = c as f64 / 255.0;
        if s <= 0.04045 { s / 12.92 } else { ((s + 0.055) / 1.055f64).powf(2.4) }
    };
    Some(0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b))
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
    let mut names: Vec<String> = BUILTIN_SKINS
        .iter()
        .map(|(n, _)| n.to_string())
        .collect();

    if let Ok(entries) = std::fs::read_dir(user_skins_dir()) {
        let mut user_names: Vec<String> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map_or(false, |ext| ext == "css"))
            .filter_map(|p| {
                p.file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_lowercase())
            })
            .collect();
        user_names.sort();
        for n in user_names {
            if !names.contains(&n) {
                names.push(n);
            }
        }
    }

    names
}

/// Load a skin by stem name, searching user files first then built-ins.
///
/// Returns `None` if no skin with that name is found.
pub fn load_skin(name: &str) -> Option<Skin> {
    let lower = name.to_lowercase();

    // Check user skins directory first.
    let user_path = user_skins_dir().join(format!("{lower}.css"));
    if user_path.exists() {
        if let Ok(css) = std::fs::read_to_string(&user_path) {
            return Some(Skin {
                name:    lower,
                css_raw: css,
                source:  SkinSource::UserFile(user_path),
            });
        }
    }

    // Fall back to built-ins.
    for (builtin_name, builtin_css) in BUILTIN_SKINS {
        if *builtin_name == lower {
            return Some(Skin {
                name:    lower,
                css_raw: builtin_css.to_string(),
                source:  SkinSource::BuiltIn,
            });
        }
    }

    None
}

/// Load a skin and prepare its CSS for use with GTK4.
///
/// Equivalent to `load_skin(name).map(|s| prepare_css(&s.css_raw, accent))`.
/// Returns `None` if the skin cannot be found.
pub fn load_prepared(name: &str, accent: &str) -> Option<String> {
    load_skin(name).map(|s| prepare_css(&s.css_raw, accent))
}

/// Load a skin from a filesystem path, bypassing the name-resolution logic.
///
/// Useful when the user has configured an absolute path to a skin file rather
/// than a skin name.  Returns `None` if the file cannot be read.
pub fn load_from_path(path: &Path) -> Option<Skin> {
    let css = std::fs::read_to_string(path).ok()?;
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("custom")
        .to_lowercase();
    Some(Skin {
        name,
        css_raw: css,
        source: SkinSource::UserFile(path.to_owned()),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The built-in dark skin must be non-empty.
    #[test]
    fn dark_skin_is_non_empty() {
        assert!(!DARK_CSS_RAW.is_empty());
    }

    /// The built-in light skin must be non-empty.
    #[test]
    fn light_skin_is_non_empty() {
        assert!(!LIGHT_CSS_RAW.is_empty());
    }

    /// Loading the "dark" skin by name must succeed and return built-in source.
    #[test]
    fn load_skin_dark_returns_builtin() {
        let skin = load_skin("dark").expect("dark skin must be available");
        assert_eq!(skin.name, "dark");
        assert!(matches!(skin.source, SkinSource::BuiltIn));
        assert!(!skin.css_raw.is_empty());
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

    /// `prepare_css` injects both @define-color declarations.
    #[test]
    fn prepare_css_injects_accent_color() {
        let out = prepare_css("body {}", "#3584e4");
        assert!(out.contains("@define-color accent_bg_color #3584e4;"));
        assert!(out.contains("@define-color accent_fg_color #ffffff;"));
        assert!(out.contains("body {}"));
    }

    /// `available_skins` always includes the two built-in skins.
    #[test]
    fn available_skins_always_includes_builtins() {
        let skins = available_skins();
        assert!(skins.contains(&"dark".to_string()));
        assert!(skins.contains(&"light".to_string()));
    }

    /// `load_prepared` returns `None` for an unknown name.
    #[test]
    fn load_prepared_unknown_returns_none() {
        assert!(load_prepared("does_not_exist", "#3584e4").is_none());
    }

    /// `load_prepared` returns Some with accent injected for a known skin.
    #[test]
    fn load_prepared_dark_contains_accent() {
        let css = load_prepared("dark", "#ed5b00").unwrap();
        assert!(css.contains("@define-color accent_bg_color #ed5b00;"));
    }

    /// Loading from an explicit path works for an existing file.
    #[test]
    fn load_from_path_works_for_existing_file() {
        // Use the skin module source file itself as a stand-in — it exists
        // and is readable; the content just won't be valid CSS.
        let path = std::path::Path::new("src/skin.rs");
        // Only run this test when the file exists (i.e. in the project root).
        if path.exists() {
            let skin = load_from_path(path).expect("should load readable file");
            assert_eq!(skin.name, "skin");
            assert!(!skin.css_raw.is_empty());
        }
    }

    /// `load_from_path` returns None for a non-existent path.
    #[test]
    fn load_from_path_missing_file_returns_none() {
        assert!(load_from_path(std::path::Path::new("/no/such/skin.css")).is_none());
    }
}
