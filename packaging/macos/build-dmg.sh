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
#  4. Code-sign the bundle and create a compressed .dmg with an /Applications
#     alias
#
# There is nothing to bundle. This script used to copy the Homebrew GStreamer
# dylibs and plug-ins in, rewrite their install names, and install a shell
# launcher that set GST_PLUGIN_PATH before gst_init(). The macOS build has no
# GStreamer in it any more. Audio is AVFoundation and discs are
# DiscRecording, both part of the OS, so all of that bundled dead weight and
# made Homebrew a build requirement for nothing.
#
# Prerequisites
# ─────────────
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

echo "==> Sparkamp macOS DMG builder — v${VERSION} (${HOST_ARCH})"
echo

# ── Step 1: Rust release build ───────────────────────────────────────────────

echo "==> [1/4] Building Rust library (release)…"
cd "$REPO_ROOT"
# Build the macOS static library (FFI bridge used by the Swift app).
cargo build --release --manifest-path frontends/macos/Cargo.toml \
    2>&1 | grep -E "^error|Finished|Compiling" | tail -3 || true
# Copy the freshly-built static lib into the Xcode project directory.
cp target/release/libsparkamp_macos.a frontends/SparkampMac/libsparkamp_macos.a

# ── Step 2: Xcode archive ────────────────────────────────────────────────────

echo "==> [2/4] Archiving Xcode project (${HOST_ARCH} only)…"
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

echo "==> [3/4] Exporting .app…"
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

# ── Code sign ────────────────────────────────────────────────────────────────
#
# SPARKAMP_SIGN_IDENTITY selects the signing mode:
#   unset / "-"        → ad-hoc (local dev builds; Gatekeeper will block
#                        downloads of these — the historical behavior).
#   "Developer ID Application: … (TEAMID)"
#                      → real signing with the hardened runtime, as
#                        notarization requires. CI sets this when the cert
#                        secret is configured; the DMG is then notarized +
#                        stapled by the workflow after this script finishes.
#
# The bundle is a single Mach-O executable now that nothing is bundled beside
# it, so sealing the app is the whole job. --deep is still never used: it is
# deprecated and mis-signs nested code.

SIGN_ID="${SPARKAMP_SIGN_IDENTITY:--}"
ENTITLEMENTS="$REPO_ROOT/packaging/macos/entitlements.plist"

if [ "$SIGN_ID" = "-" ]; then
    echo "    Ad-hoc signing…"
    codesign --force --sign - "$APP_BUNDLE" 2>/dev/null || true
else
    echo "    Signing with: $SIGN_ID"
    codesign --force --timestamp --options runtime \
        --entitlements "$ENTITLEMENTS" \
        --sign "$SIGN_ID" "$APP_BUNDLE"
    codesign --verify --strict --verbose=1 "$APP_BUNDLE"
fi

# ── Step 6: Create DMG ───────────────────────────────────────────────────────

echo "==> [4/4] Creating DMG…"
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
