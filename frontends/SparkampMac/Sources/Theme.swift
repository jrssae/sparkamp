import SwiftUI
import AppKit

// MARK: - Color + hex

extension Color {
    /// Parse a CSS hex colour string: #rgb, #rrggbb, or #rrggbbaa.
    init?(hex: String) {
        let hex = hex.trimmingCharacters(in: CharacterSet(charactersIn: "#"))
        var rgb: UInt64 = 0
        guard Scanner(string: hex).scanHexInt64(&rgb) else { return nil }
        let r, g, b, a: Double
        switch hex.count {
        case 3:
            r = Double((rgb >> 8) & 0xF) / 15
            g = Double((rgb >> 4) & 0xF) / 15
            b = Double(rgb & 0xF) / 15
            a = 1
        case 6:
            r = Double((rgb >> 16) & 0xFF) / 255
            g = Double((rgb >> 8)  & 0xFF) / 255
            b = Double(rgb         & 0xFF) / 255
            a = 1
        case 8:
            r = Double((rgb >> 24) & 0xFF) / 255
            g = Double((rgb >> 16) & 0xFF) / 255
            b = Double((rgb >> 8)  & 0xFF) / 255
            a = Double(rgb         & 0xFF) / 255
        default: return nil
        }
        self.init(red: r, green: g, blue: b, opacity: a)
    }
}

// MARK: - ButtonImageSet

/// Per-state images for a skinnable transport button.
/// Any state can be nil — nil means fall back to the SF Symbol.
struct ButtonImageSet {
    var normal:  NSImage?
    var hover:   NSImage?
    var pressed: NSImage?
}

// MARK: - SkinTheme

/// All visual values that drive the Sparkamp player UI.
/// Values come from either a built-in theme or a user-provided CSS skin file.
struct SkinTheme {
    var name: String

    // ── Player window chrome ───────────────────────────────────────────────
    var background:   Color
    var windowBorder: Color

    // ── LCD / now-playing panel ────────────────────────────────────────────
    var lcdBackground: Color
    var lcdBorder:     Color
    var titleText:     Color
    var artistText:    Color
    var timeText:      Color

    // ── Transport buttons ──────────────────────────────────────────────────
    var transportBg:       Color
    var transportBorder:   Color
    var transportText:     Color
    var transportHoverBg:  Color
    var transportActiveBg: Color
    var playButtonBg:      Color
    var playButtonText:    Color
    var playButtonBorder:  Color

    // ── Seek bar ───────────────────────────────────────────────────────────
    var seekTrack: Color
    var seekFill:  Color
    var seekThumb: Color

    // ── Volume slider ──────────────────────────────────────────────────────
    var volumeTrack: Color
    var volumeFill:  Color
    var volumeThumb: Color

    // ── Playlist window ────────────────────────────────────────────────────
    var playlistBg:           Color
    var playlistRowBg:        Color
    var playlistText:         Color
    var playlistCurrentText:  Color
    var playlistCurrentBg:    Color
    var playlistSelectedBg:   Color
    var playlistBrokenText:   Color
    var playlistDurationText: Color

    // ── Mode buttons (repeat / shuffle / playlist toggle) ──────────────────
    var modeBtnBg:         Color
    var modeBtnBorder:     Color
    var modeBtnText:       Color
    var modeBtnActiveBg:   Color
    var modeBtnActiveText: Color

    // ── Logo ───────────────────────────────────────────────────────────────
    var logoText:    Color
    var logoSubtext: Color

    // ── Button image overrides (nil entries → use SF Symbol) ───────────────
    // Keys: "prev", "rewind", "play", "pause", "stop", "ffwd", "next"
    var buttonImages: [String: ButtonImageSet] = [:]

    // Whether the theme counts as "dark" for preferredColorScheme.
    var prefersDark: Bool = true
}

// MARK: - Built-in themes

extension SkinTheme {

    // ── Dark (Winamp-inspired cyan-on-black) ───────────────────────────────
    static let defaultDark = SkinTheme(
        name: "Sparkamp Dark",
        background:        Color(hex: "#1a1a1a")!,
        windowBorder:      Color(hex: "#2a2a2a")!,
        lcdBackground:     Color(hex: "#080c0e")!,
        lcdBorder:         Color(hex: "#1e3040")!,
        titleText:         Color(hex: "#00ccff")!,
        artistText:        Color(hex: "#6a8fa0")!,
        timeText:          Color(hex: "#00ccff")!,
        transportBg:       Color(hex: "#212121")!,
        transportBorder:   Color(hex: "#363636")!,
        transportText:     Color(hex: "#aaaaaa")!,
        transportHoverBg:  Color(hex: "#2e2e2e")!,
        transportActiveBg: Color(hex: "#3a3a3a")!,
        playButtonBg:      Color(hex: "#002e3e")!,
        playButtonText:    Color(hex: "#00ccff")!,
        playButtonBorder:  Color(hex: "#005577")!,
        seekTrack:         Color(hex: "#0d1a1f")!,
        seekFill:          Color(hex: "#007799")!,
        seekThumb:         Color(hex: "#00ccff")!,
        volumeTrack:       Color(hex: "#111111")!,
        volumeFill:        Color(hex: "#004455")!,
        volumeThumb:       Color(hex: "#00aacc")!,
        playlistBg:        Color(hex: "#101010")!,
        playlistRowBg:     .clear,
        playlistText:      Color(hex: "#bbbbbb")!,
        playlistCurrentText: Color(hex: "#00ccff")!,
        playlistCurrentBg:   Color(hex: "#001a10")!,
        playlistSelectedBg:  Color(hex: "#002233")!,
        playlistBrokenText:  Color(hex: "#ff7700")!,
        playlistDurationText: Color(hex: "#3d5566")!,
        modeBtnBg:         Color(hex: "#1c1c1c")!,
        modeBtnBorder:     Color(hex: "#303030")!,
        modeBtnText:       Color(hex: "#666666")!,
        modeBtnActiveBg:   Color(hex: "#002e3e")!,
        modeBtnActiveText: Color(hex: "#00ccff")!,
        logoText:          Color(hex: "#00ccff")!,
        logoSubtext:       Color(hex: "#005566")!,
        buttonImages: [:],
        prefersDark: true
    )

    // ── Light (clean macOS-native, blue accent) ────────────────────────────
    static let defaultLight = SkinTheme(
        name: "Sparkamp Light",
        background:        Color(hex: "#ededed")!,
        windowBorder:      Color(hex: "#c8c8c8")!,
        lcdBackground:     Color(hex: "#f5f5f5")!,
        lcdBorder:         Color(hex: "#cccccc")!,
        titleText:         Color(hex: "#004e8a")!,
        artistText:        Color(hex: "#5577aa")!,
        timeText:          Color(hex: "#004e8a")!,
        transportBg:       Color(hex: "#dcdcdc")!,
        transportBorder:   Color(hex: "#b8b8b8")!,
        transportText:     Color(hex: "#444444")!,
        transportHoverBg:  Color(hex: "#cccccc")!,
        transportActiveBg: Color(hex: "#bbbbbb")!,
        playButtonBg:      Color(hex: "#cce5f7")!,
        playButtonText:    Color(hex: "#004e8a")!,
        playButtonBorder:  Color(hex: "#88bbdd")!,
        seekTrack:         Color(hex: "#cccccc")!,
        seekFill:          Color(hex: "#3388bb")!,
        seekThumb:         Color(hex: "#004e8a")!,
        volumeTrack:       Color(hex: "#d4d4d4")!,
        volumeFill:        Color(hex: "#88bbdd")!,
        volumeThumb:       Color(hex: "#3388bb")!,
        playlistBg:        Color(hex: "#f0f0f0")!,
        playlistRowBg:     .clear,
        playlistText:      Color(hex: "#333333")!,
        playlistCurrentText: Color(hex: "#004e8a")!,
        playlistCurrentBg:   Color(hex: "#ddf4e8")!,
        playlistSelectedBg:  Color(hex: "#cce0f4")!,
        playlistBrokenText:  Color(hex: "#cc5500")!,
        playlistDurationText: Color(hex: "#999999")!,
        modeBtnBg:         Color(hex: "#e0e0e0")!,
        modeBtnBorder:     Color(hex: "#bbbbbb")!,
        modeBtnText:       Color(hex: "#888888")!,
        modeBtnActiveBg:   Color(hex: "#bbdaf0")!,
        modeBtnActiveText: Color(hex: "#004e8a")!,
        logoText:          Color(hex: "#004e8a")!,
        logoSubtext:       Color(hex: "#88bbdd")!,
        buttonImages: [:],
        prefersDark: false
    )

    // ── Built-in CSS source — also the reference skin template ───────────
    /// The canonical Sparkamp dark skin in the shared portable format.
    /// This is the same content as `frontends/gtk/style_dark.css` up to the
    /// `/* GTK4 structural rules */` separator.  Skins built from this template
    /// work on Linux (GTK4) and macOS without modification.
    static let darkCSS = """
/* Sparkamp Dark Skin — portable Sparkamp Skin Format
 *
 * This file works on ALL Sparkamp frontends:
 *   • Linux  (GTK4) — :root variables are parsed by Sparkamp and translated
 *                      into GTK CSS overrides automatically.
 *   • macOS         — :root variables are parsed directly by the Swift frontend.
 *
 * To install: copy to ~/.config/sparkamp/skins/myskin.css
 *             (or load via the right-click menu on the player window)
 *
 * Button images (optional) — place PNGs next to the CSS file:
 *   .sparkamp-button-prev   { background-image: url("buttons/prev.png"); }
 *   .sparkamp-button-prev:hover   { background-image: url("buttons/prev_hover.png"); }
 *   .sparkamp-button-prev:active  { background-image: url("buttons/prev_pressed.png"); }
 *   .sparkamp-button-rewind { background-image: url("buttons/rewind.png"); }
 *   .sparkamp-button-play   { background-image: url("buttons/play.png"); }
 *   .sparkamp-button-play:hover   { background-image: url("buttons/play_hover.png"); }
 *   .sparkamp-button-play:active  { background-image: url("buttons/play_pressed.png"); }
 *   .sparkamp-button-pause  { background-image: url("buttons/pause.png"); }
 *   .sparkamp-button-stop   { background-image: url("buttons/stop.png"); }
 *   .sparkamp-button-ffwd   { background-image: url("buttons/ffwd.png"); }
 *   .sparkamp-button-next   { background-image: url("buttons/next.png"); }
 *
 * Missing button images fall back to built-in SF Symbols (macOS) or
 * the text label (Linux) — partial sets are fully supported.
 */

:root {
    /* ── Window chrome ────────────────────────────────────────────────── */
    --sparkamp-background:              #1a1a1a;
    --sparkamp-window-border:           #2a2a2a;

    /* ── LCD / Now-Playing panel ──────────────────────────────────────── */
    --sparkamp-lcd-background:          #080c0e;
    --sparkamp-lcd-border:              #1e3040;
    --sparkamp-title-text:              #00ccff;
    --sparkamp-artist-text:             #6a8fa0;
    --sparkamp-time-text:               #00ccff;

    /* ── Transport buttons ────────────────────────────────────────────── */
    --sparkamp-transport-bg:            #212121;
    --sparkamp-transport-border:        #363636;
    --sparkamp-transport-text:          #aaaaaa;
    --sparkamp-transport-hover-bg:      #2e2e2e;
    --sparkamp-transport-active-bg:     #3a3a3a;
    --sparkamp-play-button-bg:          #002e3e;
    --sparkamp-play-button-text:        #00ccff;
    --sparkamp-play-button-border:      #005577;

    /* ── Seek bar ─────────────────────────────────────────────────────── */
    --sparkamp-seek-track:              #0d1a1f;
    --sparkamp-seek-fill:               #007799;
    --sparkamp-seek-thumb:              #00ccff;

    /* ── Volume slider ────────────────────────────────────────────────── */
    --sparkamp-volume-track:            #111111;
    --sparkamp-volume-fill:             #004455;
    --sparkamp-volume-thumb:            #00aacc;

    /* ── Playlist ─────────────────────────────────────────────────────── */
    --sparkamp-playlist-bg:             #101010;
    --sparkamp-playlist-text:           #bbbbbb;
    --sparkamp-playlist-current-text:   #00ccff;
    --sparkamp-playlist-current-bg:     #001a10;
    --sparkamp-playlist-selected-bg:    #002233;
    --sparkamp-playlist-broken-text:    #ff7700;
    --sparkamp-playlist-duration-text:  #3d5566;

    /* ── Mode buttons (repeat / shuffle / playlist toggle) ────────────── */
    --sparkamp-mode-btn-bg:             #1c1c1c;
    --sparkamp-mode-btn-border:         #303030;
    --sparkamp-mode-btn-text:           #666666;
    --sparkamp-mode-btn-active-bg:      #002e3e;
    --sparkamp-mode-btn-active-text:    #00ccff;

    /* ── Logo (macOS only — Linux uses a raster logo image) ───────────── */
    --sparkamp-logo-text:               #00ccff;
    --sparkamp-logo-subtext:            #005566;
}
"""
}

// MARK: - CSS Parser

/// Parses a simplified CSS skin file into a SkinTheme.
///
/// Supported syntax:
/// - `:root { --variable: value; }` blocks for colour overrides.
/// - `.button-id { background-image: url("path.png"); }` with optional
///   `:hover` / `:active` pseudo-class suffixes for button image overrides.
///
/// All other CSS constructs are silently ignored.
enum CSSParser {

    /// Parse `css` string into a SkinTheme, applying discovered overrides
    /// on top of `base`. `skinDir` is the directory that contains the CSS
    /// file, used to resolve relative image paths.
    static func parse(css: String, skinDir: URL?, base: SkinTheme) -> SkinTheme {
        var theme = base
        let stripped = stripComments(css)
        let rules = extractRules(stripped)

        for (rawSelector, declarations) in rules {
            let selector = rawSelector.trimmingCharacters(in: .whitespacesAndNewlines)

            if selector == ":root" {
                let vars = parseDeclarations(declarations)
                applyVariables(vars, to: &theme)

            } else if selector.hasPrefix(".sparkamp-button-") {
                // e.g. ".sparkamp-button-play", ".sparkamp-button-play:hover"
                let inner = String(selector.dropFirst(17))
                let colonIdx = inner.firstIndex(of: ":")
                let buttonId = colonIdx.map { String(inner[..<$0]) } ?? inner
                let state    = colonIdx.map { String(inner[inner.index(after: $0)...]) } ?? "normal"

                guard !buttonId.isEmpty, let urlStr = parseImageURL(declarations),
                      let skinDir else { continue }
                let imgURL = skinDir.appendingPathComponent(urlStr)
                guard let img = NSImage(contentsOf: imgURL) else { continue }

                var set = theme.buttonImages[buttonId] ?? ButtonImageSet()
                switch state {
                case "hover":  set.hover   = img
                case "active": set.pressed = img
                default:       set.normal  = img
                }
                theme.buttonImages[buttonId] = set
            }
        }
        return theme
    }

    /// Convenience: load CSS from `url` and parse it.
    static func parse(url: URL, base: SkinTheme) -> SkinTheme? {
        guard let css = try? String(contentsOf: url, encoding: .utf8) else { return nil }
        return parse(css: css, skinDir: url.deletingLastPathComponent(), base: base)
    }

    // MARK: Private helpers

    private static func stripComments(_ css: String) -> String {
        var out = ""
        var idx = css.startIndex
        while idx < css.endIndex {
            let next = css.index(after: idx)
            if css[idx] == "/" && next < css.endIndex && css[next] == "*" {
                if let range = css.range(of: "*/", range: css.index(idx, offsetBy: 2)..<css.endIndex) {
                    idx = range.upperBound
                } else { break }
            } else {
                out.append(css[idx])
                idx = next
            }
        }
        return out
    }

    private static func extractRules(_ css: String) -> [(String, String)] {
        var rules: [(String, String)] = []
        var rest = css[...]
        while let open = rest.firstIndex(of: "{") {
            let selector = String(rest[..<open])
            rest = rest[rest.index(after: open)...]
            guard let close = rest.firstIndex(of: "}") else { break }
            let decls = String(rest[..<close])
            rest = rest[rest.index(after: close)...]
            rules.append((selector, decls))
        }
        return rules
    }

    private static func parseDeclarations(_ block: String) -> [String: String] {
        var result: [String: String] = [:]
        for statement in block.components(separatedBy: ";") {
            let parts = statement.split(separator: ":", maxSplits: 1)
                .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            guard parts.count == 2, !parts[0].isEmpty, !parts[1].isEmpty else { continue }
            result[parts[0]] = parts[1]
        }
        return result
    }

    private static func parseImageURL(_ declarations: String) -> String? {
        // Matches: background-image: url("some/path.png") or url('...')
        let pattern = #"background-image\s*:\s*url\s*\(\s*['"](.+?)['"]\s*\)"#
        guard let re = try? NSRegularExpression(pattern: pattern) else { return nil }
        let nsStr = declarations as NSString
        guard let m = re.firstMatch(in: declarations, range: NSRange(location: 0, length: nsStr.length)) else { return nil }
        let range = m.range(at: 1)
        guard range.location != NSNotFound else { return nil }
        return nsStr.substring(with: range)
    }

    private static func applyVariables(_ vars: [String: String], to theme: inout SkinTheme) {
        func c(_ key: String) -> Color? { vars[key].flatMap { Color(hex: $0) } }
        if let v = c("--sparkamp-background")             { theme.background        = v }
        if let v = c("--sparkamp-window-border")           { theme.windowBorder      = v }
        if let v = c("--sparkamp-lcd-background")          { theme.lcdBackground     = v }
        if let v = c("--sparkamp-lcd-border")              { theme.lcdBorder         = v }
        if let v = c("--sparkamp-title-text")              { theme.titleText         = v }
        if let v = c("--sparkamp-artist-text")             { theme.artistText        = v }
        if let v = c("--sparkamp-time-text")               { theme.timeText          = v }
        if let v = c("--sparkamp-transport-bg")            { theme.transportBg       = v }
        if let v = c("--sparkamp-transport-border")        { theme.transportBorder   = v }
        if let v = c("--sparkamp-transport-text")          { theme.transportText     = v }
        if let v = c("--sparkamp-transport-hover-bg")      { theme.transportHoverBg  = v }
        if let v = c("--sparkamp-transport-active-bg")     { theme.transportActiveBg = v }
        if let v = c("--sparkamp-play-button-bg")          { theme.playButtonBg      = v }
        if let v = c("--sparkamp-play-button-text")        { theme.playButtonText    = v }
        if let v = c("--sparkamp-play-button-border")      { theme.playButtonBorder  = v }
        if let v = c("--sparkamp-seek-track")              { theme.seekTrack         = v }
        if let v = c("--sparkamp-seek-fill")               { theme.seekFill          = v }
        if let v = c("--sparkamp-seek-thumb")              { theme.seekThumb         = v }
        if let v = c("--sparkamp-volume-track")            { theme.volumeTrack       = v }
        if let v = c("--sparkamp-volume-fill")             { theme.volumeFill        = v }
        if let v = c("--sparkamp-volume-thumb")            { theme.volumeThumb       = v }
        if let v = c("--sparkamp-playlist-bg")             { theme.playlistBg        = v }
        if let v = c("--sparkamp-playlist-text")           { theme.playlistText      = v }
        if let v = c("--sparkamp-playlist-current-text")   { theme.playlistCurrentText  = v }
        if let v = c("--sparkamp-playlist-current-bg")     { theme.playlistCurrentBg    = v }
        if let v = c("--sparkamp-playlist-selected-bg")    { theme.playlistSelectedBg   = v }
        if let v = c("--sparkamp-playlist-broken-text")    { theme.playlistBrokenText   = v }
        if let v = c("--sparkamp-playlist-duration-text")  { theme.playlistDurationText = v }
        if let v = c("--sparkamp-mode-btn-bg")             { theme.modeBtnBg         = v }
        if let v = c("--sparkamp-mode-btn-border")         { theme.modeBtnBorder     = v }
        if let v = c("--sparkamp-mode-btn-text")           { theme.modeBtnText       = v }
        if let v = c("--sparkamp-mode-btn-active-bg")      { theme.modeBtnActiveBg   = v }
        if let v = c("--sparkamp-mode-btn-active-text")    { theme.modeBtnActiveText = v }
        if let v = c("--sparkamp-logo-text")               { theme.logoText          = v }
        if let v = c("--sparkamp-logo-subtext")            { theme.logoSubtext       = v }
    }
}

// MARK: - ThemeManager

/// Manages which SkinTheme is active, reacts to system appearance changes,
/// and loads user-provided CSS skin files.
@MainActor
final class ThemeManager: ObservableObject {

    // MARK: Published state
    @Published private(set) var currentTheme: SkinTheme
    @Published private(set) var themeSource: ThemeSource

    enum ThemeSource: Equatable {
        case system, dark, light, custom(URL)
    }

    // MARK: Init
    init() {
        // Check for a persisted override first.
        let saved = UserDefaults.standard.string(forKey: "sparkamp.themeSource") ?? "system"
        let customPathStr = UserDefaults.standard.string(forKey: "sparkamp.customSkinPath")

        // Determine system appearance.
        let systemDark = NSApp.effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua

        switch saved {
        case "dark":
            themeSource  = .dark
            currentTheme = .defaultDark
        case "light":
            themeSource  = .light
            currentTheme = .defaultLight
        default:
            themeSource  = .system
            currentTheme = systemDark ? .defaultDark : .defaultLight
        }

        // Load custom skin if one was set (takes priority over dark/light/system).
        if let pathStr = customPathStr {
            let url = URL(fileURLWithPath: pathStr)
            if let custom = CSSParser.parse(url: url, base: .defaultDark) {
                currentTheme = custom
                themeSource  = .custom(url)
            }
        }

        // If no override was saved, check for a skin file in the standard locations.
        // Priority: ~/.config/sparkamp/skin.css, then skins/default.css
        if case .system = themeSource {
            let configBase = FileManager.default.homeDirectoryForCurrentUser
                .appendingPathComponent(".config/sparkamp")
            let candidates = [
                configBase.appendingPathComponent("skin.css"),
                configBase.appendingPathComponent("skins/default.css"),
            ]
            for skinURL in candidates {
                guard FileManager.default.fileExists(atPath: skinURL.path) else { continue }
                let base: SkinTheme = systemDark ? .defaultDark : .defaultLight
                if let custom = CSSParser.parse(url: skinURL, base: base) {
                    currentTheme = custom
                    themeSource  = .custom(skinURL)
                    break
                }
            }
        }
    }

    // MARK: Theme switching

    func useDark() {
        currentTheme = .defaultDark
        themeSource  = .dark
        UserDefaults.standard.set("dark", forKey: "sparkamp.themeSource")
        UserDefaults.standard.removeObject(forKey: "sparkamp.customSkinPath")
    }

    func useLight() {
        currentTheme = .defaultLight
        themeSource  = .light
        UserDefaults.standard.set("light", forKey: "sparkamp.themeSource")
        UserDefaults.standard.removeObject(forKey: "sparkamp.customSkinPath")
    }

    func useSystem(colorScheme: ColorScheme) {
        currentTheme = colorScheme == .dark ? .defaultDark : .defaultLight
        themeSource  = .system
        UserDefaults.standard.set("system", forKey: "sparkamp.themeSource")
        UserDefaults.standard.removeObject(forKey: "sparkamp.customSkinPath")
    }

    /// Called by the root view when the system appearance changes and the
    /// user hasn't pinned a specific theme.
    func systemAppearanceChanged(to colorScheme: ColorScheme) {
        guard case .system = themeSource else { return }
        currentTheme = colorScheme == .dark ? .defaultDark : .defaultLight
    }

    func loadCustomSkin(from url: URL) {
        // Detect dark/light from the skin's --sparkamp-background luminance.
        let rawCSS = (try? String(contentsOf: url, encoding: .utf8)) ?? ""
        let skinIsDark = isSkinDark(rawCSS)
        let base: SkinTheme = skinIsDark ? .defaultDark : .defaultLight
        guard let custom = CSSParser.parse(url: url, base: base) else { return }
        currentTheme = custom
        themeSource  = .custom(url)
        UserDefaults.standard.set("custom", forKey: "sparkamp.themeSource")
        UserDefaults.standard.set(url.path, forKey: "sparkamp.customSkinPath")
    }

    /// Determine whether a skin CSS string represents a dark theme by
    /// computing the relative luminance of `--sparkamp-background`.
    private func isSkinDark(_ css: String) -> Bool {
        // Quick parse: find --sparkamp-background in :root block.
        guard let rootRange = css.range(of: ":root"),
              let openBrace = css[rootRange.upperBound...].firstIndex(of: "{"),
              let closeBrace = css[openBrace...].firstIndex(of: "}") else {
            return true // default to dark
        }
        let block = css[openBrace...closeBrace]
        for line in block.components(separatedBy: ";") {
            let parts = line.split(separator: ":", maxSplits: 1)
                .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            guard parts.count == 2, parts[0] == "--sparkamp-background" else { continue }
            let hex = parts[1].trimmingCharacters(in: .whitespacesAndNewlines)
            if let color = Color(hex: hex) {
                // Use UIColor to get luminance
                var r: CGFloat = 0, g: CGFloat = 0, b: CGFloat = 0
                NSColor(color).usingColorSpace(.sRGB)?.getRed(&r, green: &g, blue: &b, alpha: nil)
                let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b
                return lum < 0.5
            }
        }
        return true
    }

    func removeCustomSkin(colorScheme: ColorScheme) {
        UserDefaults.standard.removeObject(forKey: "sparkamp.customSkinPath")
        useSystem(colorScheme: colorScheme)
    }

    func openSkinPicker(colorScheme: ColorScheme) {
        let panel = NSOpenPanel()
        panel.title = "Choose a Sparkamp Skin"
        panel.message = "Select a CSS skin file. The file must use the --sparkamp-* variable format."
        panel.allowedContentTypes = [.init(filenameExtension: "css")!]
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        // Default to ~/.config/sparkamp/skins/ if it exists.
        let skinsDir = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".config/sparkamp/skins")
        if FileManager.default.fileExists(atPath: skinsDir.path) {
            panel.directoryURL = skinsDir
        }
        panel.begin { [weak self] response in
            guard response == .OK, let url = panel.url else { return }
            Task { @MainActor in self?.loadCustomSkin(from: url) }
        }
    }

    func exportDefaultCSS() {
        let panel = NSSavePanel()
        panel.title = "Export Default Skin CSS"
        panel.nameFieldStringValue = "sparkamp_dark.css"
        panel.allowedContentTypes = [.init(filenameExtension: "css")!]
        panel.begin { response in
            guard response == .OK, let url = panel.url else { return }
            try? SkinTheme.darkCSS.write(to: url, atomically: true, encoding: .utf8)
        }
    }

    // MARK: Preferred color scheme (for SwiftUI)
    var preferredColorScheme: ColorScheme {
        currentTheme.prefersDark ? .dark : .light
    }
}

// MARK: - SkinButton

/// A transport button that supports theme colours and optional PNG image overrides.
/// Falls back to an SF Symbol when no image is provided for the button ID.
struct SkinButton: View {
    let id: String        // "prev", "rewind", "play", "pause", "stop", "ffwd", "next"
    let icon: String      // SF Symbol fallback
    let iconSize: CGFloat
    var isHighlighted: Bool = false   // true while the track is playing (play/pause btn)
    let action: () -> Void

    @EnvironmentObject var themeManager: ThemeManager
    @State private var isHovered = false
    @State private var isPressed = false

    var body: some View {
        let t = themeManager.currentTheme
        let imgSet = t.buttonImages[id]
        let img: NSImage? = isPressed
            ? (imgSet?.pressed ?? imgSet?.normal)
            : isHovered
            ? (imgSet?.hover ?? imgSet?.normal)
            : imgSet?.normal

        Button(action: action) {
            Group {
                if let img {
                    Image(nsImage: img)
                        .resizable()
                        .aspectRatio(contentMode: .fit)
                        .frame(width: iconSize + 6, height: iconSize + 6)
                } else {
                    Image(systemName: icon)
                        .font(.system(size: iconSize, weight: .semibold))
                        .foregroundStyle(isHighlighted ? t.playButtonText : t.transportText)
                        .frame(width: iconSize + 6, height: iconSize + 6)
                }
            }
            .padding(4)
            .background(
                RoundedRectangle(cornerRadius: 3)
                    .fill(isHighlighted
                          ? t.playButtonBg
                          : isPressed ? t.transportActiveBg
                          : isHovered ? t.transportHoverBg
                          : t.transportBg)
                    .overlay(
                        RoundedRectangle(cornerRadius: 3)
                            .stroke(isHighlighted ? t.playButtonBorder : t.transportBorder,
                                    lineWidth: 1)
                    )
            )
        }
        .buttonStyle(.plain)
        .onHover { isHovered = $0 }
        .simultaneousGesture(
            DragGesture(minimumDistance: 0)
                .onChanged { _ in isPressed = true }
                .onEnded   { _ in isPressed = false }
        )
    }
}

// MARK: - SparkampLogoView

struct SparkampLogoView: View {
    @EnvironmentObject var themeManager: ThemeManager

    var body: some View {
        let t = themeManager.currentTheme
        VStack(alignment: .trailing, spacing: 0) {
            HStack(spacing: 2) {
                Image(systemName: "bolt.fill")
                    .font(.system(size: 6, weight: .black))
                    .foregroundStyle(t.logoText)
                Text("SPARK")
                    .font(.system(size: 6.5, weight: .black))
                    .foregroundStyle(t.logoText)
            }
            Text("AMP")
                .font(.system(size: 6.5, weight: .black))
                .foregroundStyle(t.logoSubtext)
                .frame(maxWidth: .infinity, alignment: .trailing)
        }
        .frame(width: 40)
        .help("Sparkamp")
    }
}
