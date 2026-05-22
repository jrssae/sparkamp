import SwiftUI
import AppKit

// MARK: - Color + hex

extension Color {
    /// Parse a CSS hex colour string: #rgb, #rrggbb, or #rrggbbaa.
    init?(hex: String) {
        let hex = hex.trimmingCharacters(in: CharacterSet(charactersIn: "# \t\n\r"))
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

// MARK: - ButtonImageSet (advanced template — currently unused)
//
// Retained as an empty type so that SkinButton's `t.buttonImages[id]` lookup
// keeps compiling; the basic skin template never populates it. The advanced
// CSS layer will revive these to support per-button PNG packs.
struct ButtonImageSet {
    var normal:  NSImage?
    var hover:   NSImage?
    var pressed: NSImage?
}

// MARK: - SkinVars (mirrors Rust `SkinVars` 1:1)
//
// The 14 user-editable variables that drive all of Sparkamp's appearance.
// Same layout and defaults as `src/skin.rs` so behavior is identical
// across Linux (GTK4) and macOS for any given .css skin file.

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
    /// Built-in Dark defaults — mirrors `SkinVars::dark_defaults` in src/skin.rs.
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

    /// Built-in Light defaults — mirrors `SkinVars::light_defaults` in src/skin.rs.
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
//
// These mirror the derivations in `render_gtk_css` so a single SkinVars
// produces visually identical output on both platforms.

extension SkinVars {
    /// 18%-opacity highlight for selected list/table rows.
    var selectedRowBg: Color { highlight.opacity(0.18) }
    /// 10%-opacity highlight for the currently-playing row.
    var playingRowBg:  Color { highlight.opacity(0.10) }
    /// 8%-opacity highlight for row hover states.
    var hoverRowBg:    Color { highlight.opacity(0.08) }
    /// 60%-opacity text for muted captions (duration column, volume %).
    var dimTextColor:  Color { textColor.opacity(0.60) }

    /// Auto-derived window/panel border — ±8% luminance vs background.
    var borderColor: Color {
        let (r, g, b) = SkinVars.rgb(of: background)
        let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b
        let delta: CGFloat = lum < 0.5 ? 0.08 : -0.08
        return Color(red:   max(0, min(1, r + delta)),
                     green: max(0, min(1, g + delta)),
                     blue:  max(0, min(1, b + delta)))
    }

    /// True when the background is dark enough to use Apple's dark scheme.
    var prefersDark: Bool {
        let (r, g, b) = SkinVars.rgb(of: background)
        return (0.2126 * r + 0.7152 * g + 0.0722 * b) < 0.5
    }

    private static func rgb(of color: Color) -> (CGFloat, CGFloat, CGFloat) {
        let ns = NSColor(color).usingColorSpace(.sRGB) ?? .gray
        var r: CGFloat = 0, g: CGFloat = 0, b: CGFloat = 0
        ns.getRed(&r, green: &g, blue: &b, alpha: nil)
        return (r, g, b)
    }
}

// MARK: - Font helpers

extension SkinVars {
    /// Body font (family + standard size). Inherited as the SwiftUI default.
    var bodyFont: Font {
        .custom(fontFamily, size: fontSize)
    }
    /// Marquee title font (family + marquee size, bold).
    var marqueeFont: Font {
        .custom(fontFamily, size: fontSizeMarquee).weight(.bold)
    }
    /// Large display font for the time index — always monospaced regardless of family.
    var largeMonospaceFont: Font {
        .system(size: fontSizeLarge, weight: .regular, design: .monospaced)
    }
    /// Standard monospaced font for duration columns and volume %.
    var smallMonospaceFont: Font {
        .system(size: fontSize, design: .monospaced)
    }
}

// MARK: - AdvancedOverrides
//
// Reserved for the planned advanced-CSS layer. Each field corresponds 1:1
// with a façade property on `SkinTheme`. In the basic-template release every
// override is `nil`, so the façade falls through to derivations from
// `SkinVars`. When the advanced parser lands, populating any field here
// becomes the single change required to override the corresponding UI element
// — view code does not change, derivations stay intact.

struct AdvancedOverrides {
    var background:           Color? = nil
    var windowBorder:         Color? = nil

    var lcdBackground:        Color? = nil
    var lcdBorder:            Color? = nil
    var titleText:            Color? = nil
    var artistText:           Color? = nil
    var timeText:             Color? = nil

    var transportBg:          Color? = nil
    var transportBorder:      Color? = nil
    var transportText:        Color? = nil
    var transportHoverBg:     Color? = nil
    var transportActiveBg:    Color? = nil
    var playButtonBg:         Color? = nil
    var playButtonText:       Color? = nil
    var playButtonBorder:     Color? = nil

    var seekTrack:            Color? = nil
    var seekFill:             Color? = nil
    var seekThumb:            Color? = nil

    var volumeTrack:          Color? = nil
    var volumeFill:           Color? = nil
    var volumeThumb:          Color? = nil

    var playlistBg:           Color? = nil
    var playlistRowBg:        Color? = nil
    var playlistText:         Color? = nil
    var playlistCurrentText:  Color? = nil
    var playlistCurrentBg:    Color? = nil
    var playlistSelectedBg:   Color? = nil
    var playlistBrokenText:   Color? = nil
    var playlistDurationText: Color? = nil

    var modeBtnBg:            Color? = nil
    var modeBtnBorder:        Color? = nil
    var modeBtnText:          Color? = nil
    var modeBtnActiveBg:      Color? = nil
    var modeBtnActiveText:    Color? = nil

    var logoText:             Color? = nil
    var logoSubtext:          Color? = nil
}

// MARK: - SkinTheme façade
//
// Thin wrapper over `SkinVars` that exposes the legacy property names used
// throughout the macOS frontend. Each computed property returns the matching
// `AdvancedOverrides` slot if set, otherwise the derivation from `vars`.
//
// The façade exists for two reasons:
//  1. Property granularity — view code keeps using `theme.titleText` instead
//     of being rewritten to derive every site from raw `vars.highlight` etc.
//  2. Forward compatibility — the advanced CSS layer will populate
//     `overrides`; view code requires zero further changes.

struct SkinTheme {
    var name: String
    var vars: SkinVars
    var overrides: AdvancedOverrides

    /// Per-button image overrides. Empty in the basic template; populated by
    /// the advanced layer. SkinButton consults this map and falls back to
    /// SF Symbols when an entry is missing.
    var buttonImages: [String: ButtonImageSet] = [:]

    init(name: String,
         vars: SkinVars,
         overrides: AdvancedOverrides = AdvancedOverrides(),
         buttonImages: [String: ButtonImageSet] = [:]) {
        self.name = name
        self.vars = vars
        self.overrides = overrides
        self.buttonImages = buttonImages
    }

    // ── Window chrome ──────────────────────────────────────────────────────
    var background:   Color { overrides.background   ?? vars.background }
    var windowBorder: Color { overrides.windowBorder ?? vars.borderColor }

    // ── LCD / now-playing panel ────────────────────────────────────────────
    var lcdBackground: Color { overrides.lcdBackground ?? vars.textBackground }
    var lcdBorder:     Color { overrides.lcdBorder     ?? vars.borderColor }
    var titleText:     Color { overrides.titleText     ?? vars.highlight }
    var artistText:    Color { overrides.artistText    ?? vars.dimTextColor }
    var timeText:      Color { overrides.timeText      ?? vars.textColor }

    // ── Transport buttons ──────────────────────────────────────────────────
    var transportBg:       Color { overrides.transportBg       ?? vars.buttonColor }
    var transportBorder:   Color { overrides.transportBorder   ?? vars.borderColor }
    var transportText:     Color { overrides.transportText     ?? vars.buttonTextColor }
    var transportHoverBg:  Color { overrides.transportHoverBg  ?? vars.buttonHover }
    var transportActiveBg: Color { overrides.transportActiveBg ?? vars.buttonPressed }
    var playButtonBg:      Color { overrides.playButtonBg      ?? vars.buttonActive }
    var playButtonText:    Color { overrides.playButtonText    ?? vars.buttonTextColor }
    var playButtonBorder:  Color { overrides.playButtonBorder  ?? vars.highlight }

    // ── Seek bar ───────────────────────────────────────────────────────────
    var seekTrack: Color { overrides.seekTrack ?? vars.textBackground }
    var seekFill:  Color { overrides.seekFill  ?? vars.highlight }
    var seekThumb: Color { overrides.seekThumb ?? vars.highlight }

    // ── Volume slider ──────────────────────────────────────────────────────
    var volumeTrack: Color { overrides.volumeTrack ?? vars.textBackground }
    var volumeFill:  Color { overrides.volumeFill  ?? vars.highlight }
    var volumeThumb: Color { overrides.volumeThumb ?? vars.highlight }

    // ── Playlist window ────────────────────────────────────────────────────
    var playlistBg:           Color { overrides.playlistBg           ?? vars.textBackground }
    var playlistRowBg:        Color { overrides.playlistRowBg        ?? .clear }
    var playlistText:         Color { overrides.playlistText         ?? vars.textColor }
    var playlistCurrentText:  Color { overrides.playlistCurrentText  ?? vars.highlight }
    var playlistCurrentBg:    Color { overrides.playlistCurrentBg    ?? vars.playingRowBg }
    var playlistSelectedBg:   Color { overrides.playlistSelectedBg   ?? vars.selectedRowBg }
    var playlistBrokenText:   Color { overrides.playlistBrokenText   ?? vars.brokenColor }
    var playlistDurationText: Color { overrides.playlistDurationText ?? vars.dimTextColor }

    // ── Mode buttons ───────────────────────────────────────────────────────
    var modeBtnBg:         Color { overrides.modeBtnBg         ?? vars.buttonColor }
    var modeBtnBorder:     Color { overrides.modeBtnBorder     ?? vars.borderColor }
    var modeBtnText:       Color { overrides.modeBtnText       ?? vars.buttonTextColor }
    var modeBtnActiveBg:   Color { overrides.modeBtnActiveBg   ?? vars.buttonActive }
    var modeBtnActiveText: Color { overrides.modeBtnActiveText ?? vars.highlight }

    // ── Logo ───────────────────────────────────────────────────────────────
    var logoText:    Color { overrides.logoText    ?? vars.highlight }
    var logoSubtext: Color { overrides.logoSubtext ?? vars.dimTextColor }

    // ── Whether this skin counts as "dark" for preferredColorScheme ─────────
    var prefersDark: Bool { vars.prefersDark }
}

// MARK: - CSSParser
//
// Parses a Sparkamp skin CSS file (`:root { --sp-*: ...; }`) into a SkinVars.
// Missing or malformed variables fall back to Dark defaults per-field.
// Parsing never throws — a completely empty input yields `.dark`.

enum CSSParser {

    static func parse(css: String) -> SkinVars {
        var vars = SkinVars.dark
        let stripped = stripComments(css)
        guard let block = extractRootBlock(stripped) else { return vars }
        for statement in block.components(separatedBy: ";") {
            let trimmed = statement.trimmingCharacters(in: .whitespacesAndNewlines)
            guard trimmed.hasPrefix("--sp-") else { continue }
            let parts = trimmed.split(separator: ":", maxSplits: 1)
                .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            guard parts.count == 2 else { continue }
            apply(key: parts[0], raw: parts[1], to: &vars)
        }
        return vars
    }

    /// Convenience: load CSS from `url` and parse it.
    static func load(url: URL) -> SkinVars? {
        guard let css = try? String(contentsOf: url, encoding: .utf8) else { return nil }
        return parse(css: css)
    }

    /// Lightweight check: does the file contain a `:root { }` block?
    /// Used by `addUserSkin` to reject obviously-wrong CSS files.
    static func hasRootBlock(_ css: String) -> Bool {
        extractRootBlock(stripComments(css)) != nil
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
        case "--sp-background":        if let c = Color(hex: raw) { vars.background       = c }
        case "--sp-text-background":   if let c = Color(hex: raw) { vars.textBackground   = c }
        case "--sp-text-color":        if let c = Color(hex: raw) { vars.textColor        = c }
        case "--sp-highlight":         if let c = Color(hex: raw) { vars.highlight        = c }
        case "--sp-broken-color":      if let c = Color(hex: raw) { vars.brokenColor      = c }
        case "--sp-button-color":      if let c = Color(hex: raw) { vars.buttonColor      = c }
        case "--sp-button-hover":      if let c = Color(hex: raw) { vars.buttonHover      = c }
        case "--sp-button-active":     if let c = Color(hex: raw) { vars.buttonActive     = c }
        case "--sp-button-pressed":    if let c = Color(hex: raw) { vars.buttonPressed    = c }
        case "--sp-button-text-color": if let c = Color(hex: raw) { vars.buttonTextColor  = c }
        case "--sp-font-family":       vars.fontFamily = stripQuotes(raw)
        case "--sp-font-size":         if let n = parsePx(raw) { vars.fontSize        = n }
        case "--sp-font-size-large":   if let n = parsePx(raw) { vars.fontSizeLarge   = n }
        case "--sp-font-size-marquee": if let n = parsePx(raw) { vars.fontSizeMarquee = n }
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

// MARK: - ThemeManager
//
// Owns the active skin, persists the user's choice, and exposes a registry
// of available skins (built-ins + user-dir CSS files, minus hidden entries).
// API mirrors the Rust `skin.rs` surface so cross-platform reasoning stays
// straightforward.

@MainActor
final class ThemeManager: ObservableObject {

    // MARK: Published state
    @Published private(set) var currentVars: SkinVars
    @Published private(set) var activeSkin:  String   // "dark" | "light" | user stem

    // MARK: Storage keys
    private static let activeSkinKey  = "sparkamp.activeSkin"
    private static let hiddenSkinsKey = "sparkamp.hiddenSkins"

    // MARK: Init
    init() {
        let saved = UserDefaults.standard.string(forKey: Self.activeSkinKey) ?? "dark"
        self.activeSkin  = saved
        self.currentVars = Self.load(skinName: saved) ?? .dark
        Self.publishSelectionColor(SkinTheme(name: saved, vars: self.currentVars))
    }

    /// Push the active skin's full-opacity highlight colour into the AppKit
    /// selection palette so NSTableRowView's swizzled `drawSelection(in:)`
    /// paints rows with the skin's highlight colour.  The 18% alpha is
    /// applied at draw time (see `SparkampSelectionPalette.rowHighlightAlpha`)
    /// — publishing the full-opacity colour avoids the
    /// SwiftUI Color → NSColor bridge silently dropping alpha for
    /// non-system colours, which produced an over-saturated selection bar
    /// when alpha was baked into the published colour.
    private static func publishSelectionColor(_ theme: SkinTheme) {
        SparkampSelectionPalette.rowHighlight = NSColor(theme.vars.highlight)
    }

    // MARK: Façade access
    /// Wrap currentVars in a SkinTheme façade for legacy view code.
    /// Computed on each read — `vars` is the source of truth.
    var currentTheme: SkinTheme {
        SkinTheme(name: activeSkin, vars: currentVars)
    }

    /// Colour scheme hint for SwiftUI's `.preferredColorScheme` modifier.
    var preferredColorScheme: ColorScheme {
        currentVars.prefersDark ? .dark : .light
    }

    // MARK: Skin registry

    struct SkinEntry: Identifiable, Equatable {
        var name: String             // "dark", "light", or user stem
        var displayName: String
        var isBuiltin: Bool
        var path: URL?
        var id: String { name }
    }

    /// Built-ins + user-dir `.css` files, minus hidden entries.
    /// Built-ins are never hidden.
    func listSkins() -> [SkinEntry] {
        var out: [SkinEntry] = [
            SkinEntry(name: "dark",  displayName: "Dark",  isBuiltin: true, path: nil),
            SkinEntry(name: "light", displayName: "Light", isBuiltin: true, path: nil),
        ]
        let dir = Self.userSkinsDir()
        let hidden = Set((UserDefaults.standard.stringArray(forKey: Self.hiddenSkinsKey) ?? [])
            .map { $0.lowercased() })
        if let urls = try? FileManager.default.contentsOfDirectory(
            at: dir, includingPropertiesForKeys: nil) {
            let sorted = urls
                .filter { $0.pathExtension.lowercased() == "css" }
                .sorted { $0.lastPathComponent.lowercased() < $1.lastPathComponent.lowercased() }
            for url in sorted {
                let stem = url.deletingPathExtension().lastPathComponent.lowercased()
                if hidden.contains(stem) { continue }
                out.append(SkinEntry(
                    name: stem,
                    displayName: titlecaseSkinStem(stem),
                    isBuiltin: false,
                    path: url))
            }
        }
        return out
    }

    // MARK: Active skin

    func setActiveSkin(_ name: String) {
        let lowered = name.lowercased()
        let vars = Self.load(skinName: lowered) ?? .dark
        self.activeSkin  = lowered
        self.currentVars = vars
        Self.publishSelectionColor(SkinTheme(name: lowered, vars: vars))
        UserDefaults.standard.set(lowered, forKey: Self.activeSkinKey)
    }

    // MARK: Add / Hide

    enum AddSkinError: Error {
        case unreadable
        case noRootBlock
        case copyFailed
    }

    @discardableResult
    func addUserSkin(from source: URL) -> Result<SkinEntry, AddSkinError> {
        let dir = Self.userSkinsDir()
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)

        guard let css = try? String(contentsOf: source, encoding: .utf8) else {
            return .failure(.unreadable)
        }
        guard CSSParser.hasRootBlock(css) else {
            return .failure(.noRootBlock)
        }

        let stem = source.deletingPathExtension().lastPathComponent.lowercased()
        let (finalStem, dest) = uniquify(dir: dir, stem: stem)
        do {
            try FileManager.default.copyItem(at: source, to: dest)
        } catch {
            return .failure(.copyFailed)
        }

        // Un-hide if it was previously hidden.
        var hidden = UserDefaults.standard.stringArray(forKey: Self.hiddenSkinsKey) ?? []
        hidden.removeAll { $0.caseInsensitiveCompare(finalStem) == .orderedSame }
        UserDefaults.standard.set(hidden, forKey: Self.hiddenSkinsKey)

        return .success(SkinEntry(
            name: finalStem,
            displayName: titlecaseSkinStem(finalStem),
            isBuiltin: false,
            path: dest))
    }

    /// Remove a skin.  For user-installed skins this DELETES the `.css` file
    /// from disk so the next add of the same source file is treated as brand
    /// new (no "-2"/"-3" suffix).  Built-in skins (dark/light) have no file
    /// — their CSS is embedded — so they are hidden via UserDefaults instead.
    ///
    /// This intentionally diverges from Sparkamp's general "removing from UI
    /// must not delete from disk" policy: a user skin file lives entirely in
    /// our managed `~/Library/Application Support/Sparkamp/skins/` directory
    /// and is functionally equivalent to a registry entry — keeping the file
    /// after Remove just creates name-collision noise on re-add.
    func hideSkin(_ name: String) {
        let lowered = name.lowercased()

        // Drop active-skin pointer first so we don't keep a deleted file selected.
        if activeSkin == lowered {
            setActiveSkin("dark")
        }

        // User skin → delete the file outright; clear any stale hidden entry.
        let userFile = Self.userSkinsDir().appendingPathComponent("\(lowered).css")
        if FileManager.default.fileExists(atPath: userFile.path) {
            try? FileManager.default.removeItem(at: userFile)
            var hidden = UserDefaults.standard.stringArray(forKey: Self.hiddenSkinsKey) ?? []
            hidden.removeAll { $0.caseInsensitiveCompare(lowered) == .orderedSame }
            UserDefaults.standard.set(hidden, forKey: Self.hiddenSkinsKey)
            return
        }

        // Built-in (no file on disk) → hide via UserDefaults.  Dark stays
        // un-hideable so the app always has a fallback skin.
        guard lowered != "dark" else { return }
        var hidden = UserDefaults.standard.stringArray(forKey: Self.hiddenSkinsKey) ?? []
        if !hidden.contains(where: { $0.caseInsensitiveCompare(lowered) == .orderedSame }) {
            hidden.append(lowered)
            UserDefaults.standard.set(hidden, forKey: Self.hiddenSkinsKey)
        }
    }

    // MARK: Export

    /// Write a copy of the named skin to `destination`. For built-ins this
    /// emits the embedded template literal; for user skins it copies the
    /// installed `.css` file verbatim.
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

    /// Write the skin guide markdown to `destination`. Content is the same
    /// as `src/skin_templates/skin-guide.md` (auto-embedded at build time).
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
            // CSSParser.load already returns nil on read failure (missing
            // file, permission error, etc.) — no need to pre-check existence.
            let path = userSkinsDir().appendingPathComponent("\(skinName).css")
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

    // MARK: Embedded template literals
    //
    // Source-of-truth lives in `src/skin_templates/dark.css` / `light.css`.
    // These literals are kept in sync by hand for now; the advanced template
    // phase will replace them with bundle-loaded copies.

    static let darkTemplateCSS: String = """
    /* Sparkamp Dark — Basic Skin Template
     *
     * Edit these 14 values and save this file to
     * ~/.config/sparkamp/skins/<name>.css, then load it from
     * Settings → Appearance → Add skin…
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
    """

    static let lightTemplateCSS: String = """
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
    """

    // `skinGuideMD` is generated by tools/embed-skin-guide.swift into
    // `Sources/Theme+Guide.swift` (Xcode build phase). The generator keeps
    // it in sync with `src/skin_templates/skin-guide.md` — the canonical copy.
}

// MARK: - Helpers

/// "midnight-teal" → "Midnight Teal".  Mirrors Rust `titlecase` in skin.rs.
private func titlecaseSkinStem(_ stem: String) -> String {
    stem.split(whereSeparator: { $0 == "-" || $0 == "_" })
        .filter { !$0.isEmpty }
        .map { $0.prefix(1).uppercased() + $0.dropFirst() }
        .joined(separator: " ")
}

// MARK: - SkinButton

/// A transport button that supports theme colours and (in the advanced
/// template) optional PNG image overrides. Falls back to an SF Symbol when
/// no image is provided for the button ID.
struct SkinButton: View {
    let id: String
    let icon: String
    let iconSize: CGFloat
    var isHighlighted: Bool = false
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
