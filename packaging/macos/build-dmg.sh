#!/bin/bash
# packaging/macos/build-dmg.sh
#
# Builds a fully self-contained Sparkamp.dmg for macOS.
#
# What it does
# ─────────────
#  1. cargo build --release    (Rust static library, current architecture)
#  2. xcodebuild archive       (Swift app, Release config, same architecture)
#  3. Export the .app from the archive
#  4. Bundle all Homebrew GStreamer dylibs into Contents/Frameworks/
#     using a recursive otool walk + install_name_tool rewrites
#  5. Bundle the required GStreamer audio plug-ins
#  6. Write a thin launcher script so GST_PLUGIN_PATH is set before gst_init()
#  7. Ad-hoc code-sign the bundle
#  8. Create a compressed .dmg with an /Applications alias
#
# Prerequisites
# ─────────────
#   brew install gstreamer gst-plugins-base gst-plugins-good \
#                gst-plugins-bad gst-plugins-ugly gst-libav mpg123
#   Xcode Command Line Tools  (xcode-select --install)
#   Rust  (curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh)
#
# Usage
# ─────
#   cd /path/to/Sparkamp
#   bash packaging/macos/build-dmg.sh
#   # → dist/Sparkamp-<version>.dmg

set -euo pipefail

# ── Config ───────────────────────────────────────────────────────────────────

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
XCODEPROJ="$REPO_ROOT/frontends/SparkampMac/SparkampMac.xcodeproj"
SCHEME="SparkampMac"
APP_NAME="SparkampMac"
BUNDLE_NAME="Sparkamp"

VERSION="$(grep 'MARKETING_VERSION' "$XCODEPROJ/project.pbxproj" \
           | head -1 | sed 's/.*= //;s/;//;s/ //')"

# Guard against version drift: the DMG name and the app's CFBundleShortVersionString
# both come from MARKETING_VERSION above, while the Rust core and Flatpak read
# Cargo.toml. If a release bump touches one but not the other (as happened at
# v1.0.2 and v1.1.0), the DMG ships mislabeled. Fail the build instead of
# producing a wrongly-versioned asset; `scripts/sync-version.sh` fixes drift.
CARGO_VERSION="$(grep -E '^version = "' "$REPO_ROOT/Cargo.toml" \
                 | head -1 | sed -E 's/^version = "([^"]+)".*/\1/')"
if [[ "$VERSION" != "$CARGO_VERSION" ]]; then
  echo "error: version drift — MARKETING_VERSION ($VERSION) != Cargo.toml ($CARGO_VERSION)." >&2
  echo "       Run scripts/sync-version.sh before building." >&2
  exit 1
fi

ARCHIVE_PATH="/tmp/${APP_NAME}.xcarchive"
EXPORT_DIR="/tmp/${APP_NAME}_export"
EXPORT_PLIST="/tmp/${APP_NAME}_export_options.plist"
DIST_DIR="$REPO_ROOT/dist"
DMG_DIR="/tmp/${APP_NAME}_dmg"
DMG_NAME="${BUNDLE_NAME}-${VERSION}.dmg"

HOST_ARCH="$(uname -m)"   # arm64 on Apple Silicon, x86_64 on Intel

BREW_GST_PLUGINS="/opt/homebrew/lib/gstreamer-1.0"

# GStreamer plug-ins required for audio playback + EQ + spectrum visualiser
REQUIRED_PLUGINS="
libgstcoreelements.dylib
libgstplayback.dylib
libgsttypefindfunctions.dylib
libgstaudioconvert.dylib
libgstaudioresample.dylib
libgstvolume.dylib
libgstautodetect.dylib
libgstosxaudio.dylib
libgstequalizer.dylib
libgstspectrum.dylib
libgstaudioparsers.dylib
libgstaudiofx.dylib
libgstid3demux.dylib
libgstapetag.dylib
libgstflac.dylib
libgstogg.dylib
libgstvorbis.dylib
libgstopus.dylib
libgstmpg123.dylib
libgstwavparse.dylib
libgstapp.dylib
"

echo "==> Sparkamp macOS DMG builder — v${VERSION} (${HOST_ARCH})"
echo

# ── Step 1: Rust release build ───────────────────────────────────────────────

echo "==> [1/6] Building Rust library (release)…"
cd "$REPO_ROOT"
# Build the macOS static library (FFI bridge used by the Swift app).
cargo build --release --manifest-path frontends/macos/Cargo.toml \
    2>&1 | grep -E "^error|Finished|Compiling" | tail -3 || true
# Copy the freshly-built static lib into the Xcode project directory.
cp target/release/libsparkamp_macos.a frontends/SparkampMac/libsparkamp_macos.a

# ── Step 2: Xcode archive ────────────────────────────────────────────────────

echo "==> [2/6] Archiving Xcode project (${HOST_ARCH} only)…"
rm -rf "$ARCHIVE_PATH"
# Capture the full log and fail loudly on error: piping xcodebuild straight
# into a grep filter (the old approach) hid compile errors — Swift errors
# print as "File.swift:line: error:", which a "^error:" filter drops, so a
# broken archive looked like a silent no-op and only surfaced two steps
# later as "archive not found".
ARCHIVE_LOG="$(mktemp -t sparkamp-archive)"
set +e
xcodebuild \
    -project "$XCODEPROJ" \
    -scheme "$SCHEME" \
    -configuration Release \
    -archivePath "$ARCHIVE_PATH" \
    -destination "generic/platform=macOS" \
    ARCHS="$HOST_ARCH" \
    ONLY_ACTIVE_ARCH=YES \
    archive \
    CODE_SIGN_IDENTITY="-" \
    CODE_SIGNING_REQUIRED=NO \
    CODE_SIGNING_ALLOWED=NO \
    > "$ARCHIVE_LOG" 2>&1
archive_rc=$?
set -e
if [ $archive_rc -ne 0 ] || [ ! -d "$ARCHIVE_PATH" ]; then
    echo "ERROR: xcodebuild archive failed. Diagnostics:" >&2
    grep -E "error:|warning: .*error|ld: " "$ARCHIVE_LOG" | tail -30 >&2 \
        || tail -40 "$ARCHIVE_LOG" >&2
    exit 1
fi
echo "    Archive complete."

# ── Step 3: Export .app ──────────────────────────────────────────────────────

echo "==> [3/6] Exporting .app…"
rm -rf "$EXPORT_DIR"
cat > "$EXPORT_PLIST" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>method</key>
    <string>mac-application</string>
    <key>signingStyle</key>
    <string>manual</string>
    <key>signingCertificate</key>
    <string>-</string>
</dict>
</plist>
PLIST

EXPORT_LOG="$(mktemp -t sparkamp-export)"
set +e
xcodebuild \
    -exportArchive \
    -archivePath "$ARCHIVE_PATH" \
    -exportPath "$EXPORT_DIR" \
    -exportOptionsPlist "$EXPORT_PLIST" \
    CODE_SIGN_IDENTITY="-" \
    CODE_SIGNING_REQUIRED=NO \
    > "$EXPORT_LOG" 2>&1
export_rc=$?
set -e
if [ $export_rc -ne 0 ]; then
    echo "ERROR: xcodebuild -exportArchive failed. Diagnostics:" >&2
    grep -E "error:|EXPORT FAILED" "$EXPORT_LOG" | tail -20 >&2 \
        || tail -30 "$EXPORT_LOG" >&2
    exit 1
fi

APP_BUNDLE="$(find "$EXPORT_DIR" -name "*.app" -maxdepth 2 | head -1)"
if [ -z "$APP_BUNDLE" ]; then
    echo "ERROR: could not find exported .app in $EXPORT_DIR"
    exit 1
fi
echo "    Found: $APP_BUNDLE"

FRAMEWORKS_DIR="$APP_BUNDLE/Contents/Frameworks"
# GStreamer plug-ins live under Resources, NOT Frameworks. codesign, sealing
# the app, treats every entry directly under Contents/Frameworks as nested
# code and expects a dylib or a real .framework; a plain plug-in directory
# there ("gstreamer-1.0") fails Developer ID signing with "bundle format
# unrecognized … In subcomponent: …/Frameworks/gstreamer-1.0". Resources is
# not auto-descended as a nested bundle, so loose plug-in dylibs there sign
# cleanly (each is still individually signed). Dep references use
# @executable_path/../Frameworks, which resolves the same from either dir.
PLUGINS_DIR="$APP_BUNDLE/Contents/Resources/gstreamer-1.0"
MACOS_DIR="$APP_BUNDLE/Contents/MacOS"

mkdir -p "$FRAMEWORKS_DIR"
mkdir -p "$PLUGINS_DIR"

# ── Step 4: Bundle GStreamer dylibs ──────────────────────────────────────────

echo "==> [4/6] Bundling GStreamer dylibs (recursive)…"

# bundle_dylib <path-to-src-dylib>
# Copies the dylib into Frameworks/ (if not already there) then recurses
# into its own Homebrew dependencies.  Uses the Frameworks/ dir itself as
# the "visited" set — if the file already exists there, skip it.
bundle_dylib() {
    local src="$1"
    # Resolve to real path (follow symlinks)
    local real
    real="$(cd "$(dirname "$src")" && pwd -P)/$(basename "$src")" 2>/dev/null || return
    [ -f "$real" ] || return

    local name
    name="$(basename "$real")"
    # Already bundled?
    [ -f "$FRAMEWORKS_DIR/$name" ] && return

    cp -f "$real" "$FRAMEWORKS_DIR/$name"

    # Recurse into this lib's Homebrew dependencies
    otool -L "$real" 2>/dev/null | tail -n +2 | awk '{print $1}' | while read -r dep; do
        case "$dep" in
            /opt/homebrew/*|/usr/local/*) bundle_dylib "$dep" ;;
        esac
    done
}

# Seed from all binaries in the MacOS/ directory
for bin in "$MACOS_DIR"/*; do
    [ -f "$bin" ] || continue
    otool -L "$bin" 2>/dev/null | tail -n +2 | awk '{print $1}' | while read -r dep; do
        case "$dep" in
            /opt/homebrew/*|/usr/local/*) bundle_dylib "$dep" ;;
        esac
    done
done

echo "    Bundled $(ls "$FRAMEWORKS_DIR"/*.dylib 2>/dev/null | wc -l | tr -d ' ') dylibs."

# ── Rewrite install names in Frameworks/ ─────────────────────────────────────

echo "    Rewriting install names…"
FWPATH="@executable_path/../Frameworks"

rewrite_binary() {
    local bin="$1"
    # Give the dylib its new identity
    if echo "$bin" | grep -q "\.dylib"; then
        install_name_tool -id "@rpath/$(basename "$bin")" "$bin" 2>/dev/null || true
    fi
    # Rewrite each Homebrew dep reference
    otool -L "$bin" 2>/dev/null | tail -n +2 | awk '{print $1}' | while read -r dep; do
        case "$dep" in
            /opt/homebrew/*|/usr/local/*)
                install_name_tool -change "$dep" "$FWPATH/$(basename "$dep")" "$bin" 2>/dev/null || true
                ;;
        esac
    done
}

for lib in "$FRAMEWORKS_DIR"/*.dylib; do
    [ -f "$lib" ] || continue
    rewrite_binary "$lib"
done

for bin in "$MACOS_DIR"/*; do
    [ -f "$bin" ] || continue
    install_name_tool -add_rpath "$FWPATH" "$bin" 2>/dev/null || true
    rewrite_binary "$bin"
done

# ── Step 5: Bundle GStreamer plug-ins ────────────────────────────────────────

echo "==> [5/6] Bundling GStreamer plug-ins…"

for plugin in $REQUIRED_PLUGINS; do
    plugin="$(echo "$plugin" | tr -d '[:space:]')"
    [ -z "$plugin" ] && continue
    src="$BREW_GST_PLUGINS/$plugin"
    if [ ! -f "$src" ]; then
        echo "    SKIP (missing): $plugin"
        continue
    fi
    cp -f "$src" "$PLUGINS_DIR/$plugin"

    # Also bundle any Homebrew deps of this plugin
    otool -L "$src" 2>/dev/null | tail -n +2 | awk '{print $1}' | while read -r dep; do
        case "$dep" in
            /opt/homebrew/*|/usr/local/*) bundle_dylib "$dep" ;;
        esac
    done
done

# Rewrite plug-in install names
for plugin in "$PLUGINS_DIR"/*.dylib; do
    [ -f "$plugin" ] || continue
    install_name_tool -id "@rpath/gstreamer-1.0/$(basename "$plugin")" "$plugin" 2>/dev/null || true
    otool -L "$plugin" 2>/dev/null | tail -n +2 | awk '{print $1}' | while read -r dep; do
        case "$dep" in
            /opt/homebrew/*|/usr/local/*)
                install_name_tool -change "$dep" "$FWPATH/$(basename "$dep")" "$plugin" 2>/dev/null || true
                ;;
        esac
    done
done

# Final pass: any new Frameworks dylibs added by plugin deps need their names rewritten too
for lib in "$FRAMEWORKS_DIR"/*.dylib; do
    [ -f "$lib" ] || continue
    rewrite_binary "$lib"
done

echo "    $(ls "$PLUGINS_DIR"/*.dylib 2>/dev/null | wc -l | tr -d ' ') plug-ins bundled."

# ── Launcher wrapper ──────────────────────────────────────────────────────────
# GStreamer must find its plug-ins via GST_PLUGIN_PATH before gst_init().
# We rename the real binary to SparkampMac.bin and put a thin shell launcher
# in its place that sets the variable, then exec's the real binary.

echo "    Writing GST_PLUGIN_PATH launcher…"
REAL_BIN="$MACOS_DIR/${APP_NAME}.bin"
mv "$MACOS_DIR/${APP_NAME}" "$REAL_BIN"

cat > "$MACOS_DIR/${APP_NAME}" <<'LAUNCHER'
#!/bin/bash
DIR="$(cd "$(dirname "$0")" && pwd)"
export GST_PLUGIN_PATH="$DIR/../Resources/gstreamer-1.0"
export GST_PLUGIN_SYSTEM_PATH=""
export GIO_EXTRA_MODULES=""
exec "$DIR/SparkampMac.bin" "$@"
LAUNCHER
chmod +x "$MACOS_DIR/${APP_NAME}"

# ── Code sign ────────────────────────────────────────────────────────────────
#
# SPARKAMP_SIGN_IDENTITY selects the signing mode:
#   unset / "-"        → ad-hoc (local dev builds; Gatekeeper will block
#                        downloads of these — the historical behavior).
#   "Developer ID Application: … (TEAMID)"
#                      → real signing with the hardened runtime + the
#                        entitlements GStreamer needs, as notarization
#                        requires. CI sets this when the cert secret is
#                        configured; the DMG is then notarized + stapled
#                        by the workflow after this script finishes.
#
# Order matters for real signatures: every Mach-O leaf (dylibs, GStreamer
# plug-ins, the real binary) gets its own signature first, the bundle is
# sealed last, and --deep is never used (it's deprecated and mis-signs
# nested code; explicit inside-out signing is the supported way).

SIGN_ID="${SPARKAMP_SIGN_IDENTITY:--}"
ENTITLEMENTS="$REPO_ROOT/packaging/macos/entitlements.plist"

if [ "$SIGN_ID" = "-" ]; then
    echo "    Ad-hoc signing…"
    find "$APP_BUNDLE" \( -name "*.dylib" -o -name "*.so" \) -print0 \
        | xargs -0 -I{} codesign --force --sign - {} 2>/dev/null || true
    codesign --force --deep --sign - "$APP_BUNDLE" 2>/dev/null || true
else
    echo "    Signing with: $SIGN_ID"
    # Leaves: bundled dylibs + GStreamer plug-ins. Same identity as the app,
    # so hardened-runtime library validation accepts them without a
    # disable-library-validation entitlement.
    find "$APP_BUNDLE" \( -name "*.dylib" -o -name "*.so" \) -print0 \
        | xargs -0 -I{} codesign --force --timestamp --options runtime \
            --sign "$SIGN_ID" {}
    # The real executable (the launcher shell script is sealed as a bundle
    # resource — scripts don't carry Mach-O signatures).
    codesign --force --timestamp --options runtime \
        --entitlements "$ENTITLEMENTS" \
        --sign "$SIGN_ID" "$MACOS_DIR/SparkampMac.bin"
    # Seal the bundle.
    codesign --force --timestamp --options runtime \
        --entitlements "$ENTITLEMENTS" \
        --sign "$SIGN_ID" "$APP_BUNDLE"
    codesign --verify --strict --verbose=1 "$APP_BUNDLE"
fi

# ── Step 6: Create DMG ───────────────────────────────────────────────────────

echo "==> [6/6] Creating DMG…"
mkdir -p "$DIST_DIR"
rm -rf "$DMG_DIR"
mkdir -p "$DMG_DIR"

cp -R "$APP_BUNDLE" "$DMG_DIR/"
ln -sf /Applications "$DMG_DIR/Applications"

DMG_TEMP="/tmp/${APP_NAME}_rw.dmg"
rm -f "$DMG_TEMP"

hdiutil create \
    -volname "$BUNDLE_NAME" \
    -srcfolder "$DMG_DIR" \
    -ov \
    -format UDRW \
    "$DMG_TEMP" 2>&1 | tail -2

rm -f "$DIST_DIR/$DMG_NAME"
hdiutil convert \
    "$DMG_TEMP" \
    -format UDZO \
    -o "$DIST_DIR/$DMG_NAME" 2>&1 | tail -2

rm -f "$DMG_TEMP"

echo
echo "╔══════════════════════════════════════════════════════════╗"
echo "║  ✅  Build complete                                      ║"
printf "║  📦  %-52s  ║\n" "dist/$DMG_NAME"
printf "║  📐  %-52s  ║\n" "$(du -sh "$DIST_DIR/$DMG_NAME" | cut -f1) on disk"
echo "╚══════════════════════════════════════════════════════════╝"
echo
echo "Installation:"
echo "  1. Open the DMG and drag Sparkamp into Applications."
if [ "$SIGN_ID" = "-" ]; then
    echo "  2. Ad-hoc build — macOS will block the first launch. Approve via"
    echo "     System Settings → Privacy & Security → Open Anyway, or run:"
    echo "       xattr -dr com.apple.quarantine /Applications/SparkampMac.app"
else
    echo "  2. Developer ID signed. After the workflow notarizes + staples"
    echo "     the DMG, downloads open without Gatekeeper prompts."
fi
