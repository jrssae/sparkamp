#!/bin/bash
# packaging/macos/build-appstore.sh
#
# Builds a signed Sparkamp .pkg for the Mac App Store.
#
# What it does
# ─────────────
#  1. cargo build --release        (Rust static library)
#  2. xcodebuild archive           (Swift app, Release, sandbox entitlements,
#                                   real signing — not the ad-hoc signing the
#                                   DMG build uses)
#  3. xcodebuild -exportArchive    (produces the .pkg to upload)
#  4. Verifies the result: sandboxed, no GStreamer, right bundle id
#
# What it deliberately does NOT do
# ─────────────────────────────────
#  Bundle GStreamer. The DMG build carries ~40 MB of dylibs, a plug-in tree and
#  a shell-script launcher so `gst_init` can find them. The App Store build
#  ships none of it — playback, burning, ripping and duration probing all reach
#  AVFoundation, and the crate does not link GStreamer on macOS at all. Step 4
#  checks that rather than trusting it.
#
# Prerequisites you must set up yourself — see the checklist this prints on
# failure, or docs/superpowers/plans/2026-09-02-app-store-signing.md.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
APP_NAME="SparkampMac"
BUNDLE_NAME="Sparkamp"
BUNDLE_ID="dev.sparkamp.Sparkamp"
TEAM_ID="HR3P54M383"
SCHEME="$APP_NAME"
XCODEPROJ="$REPO_ROOT/frontends/SparkampMac/$APP_NAME.xcodeproj"
ENTITLEMENTS="$SCRIPT_DIR/entitlements-appstore.plist"
EXPORT_PLIST="$SCRIPT_DIR/export-options-appstore.plist"

ARCHIVE_PATH="/tmp/${APP_NAME}-appstore.xcarchive"
EXPORT_DIR="/tmp/${APP_NAME}-appstore-export"
DIST_DIR="$REPO_ROOT/dist"

VERSION="$(grep 'MARKETING_VERSION' "$XCODEPROJ/project.pbxproj" \
           | head -1 | sed 's/.*= //;s/;//;s/ //')"
CARGO_VERSION="$(grep -E '^version = "' "$REPO_ROOT/Cargo.toml" \
                 | head -1 | sed -E 's/^version = "([^"]+)".*/\1/')"
if [[ "$VERSION" != "$CARGO_VERSION" ]]; then
  echo "error: version drift — MARKETING_VERSION ($VERSION) != Cargo.toml ($CARGO_VERSION)." >&2
  echo "       Run scripts/sync-version.sh before building." >&2
  exit 1
fi

say() { printf '\n==> %s\n' "$*"; }

missing_prerequisites() {
  cat >&2 <<'MSG'

The App Store signing chain is not set up on this machine. It needs three
things that only an Apple Developer account holder can create, and none of
them can be scripted:

  1. An "Apple Distribution" certificate, in this team.
       Xcode → Settings → Accounts → Manage Certificates → + → Apple Distribution
       (The "Developer ID Application" certificates already here are for the
        DMG. They are not accepted for App Store submission.)

  2. An App ID for dev.sparkamp.Sparkamp, at
       https://developer.apple.com/account/resources/identifiers
       Enable no extra capabilities — the sandbox entitlements this build uses
       need none of them.

  3. A "Mac App Store" provisioning profile named exactly
       Sparkamp Mac App Store
     for that App ID and that certificate, at
       https://developer.apple.com/account/resources/profiles
     Download it and double-click to install.

  For upload you will also want a "Mac Installer Distribution" certificate,
  but the export in step 3 does not need it.

Then run this script again. It re-checks and will tell you which of the three
is still missing.
MSG
  exit 1
}

say "[0/4] Checking the signing chain"
have_dist_cert=0
if security find-identity -v -p codesigning 2>/dev/null | grep -q "Apple Distribution.*$TEAM_ID"; then
  have_dist_cert=1
  echo "  Apple Distribution certificate: found"
else
  echo "  Apple Distribution certificate: MISSING"
fi
profile_dir="$HOME/Library/Developer/Xcode/UserData/Provisioning Profiles"
have_profile=0
if [ -d "$profile_dir" ] && grep -rlq "$BUNDLE_ID" "$profile_dir" 2>/dev/null; then
  have_profile=1
  echo "  Provisioning profile for $BUNDLE_ID: found"
else
  echo "  Provisioning profile for $BUNDLE_ID: MISSING"
fi
if [ "$have_dist_cert" -eq 0 ] || [ "$have_profile" -eq 0 ]; then
  missing_prerequisites
fi

say "[1/4] Building the Rust static library (release)"
cd "$REPO_ROOT"
cargo build --release --manifest-path frontends/macos/Cargo.toml
cp target/release/libsparkamp_macos.a frontends/SparkampMac/libsparkamp_macos.a

say "[2/4] Archiving (Release, sandboxed, signed)"
rm -rf "$ARCHIVE_PATH"
ARCHIVE_LOG="$(mktemp -t sparkamp-appstore-archive)"
set +e
# Signed for real, unlike the DMG build. An App Store archive that was signed
# ad-hoc and re-signed afterwards loses its provisioning profile, and the
# failure surfaces at upload as a generic rejection.
xcodebuild \
    -project "$XCODEPROJ" \
    -scheme "$SCHEME" \
    -configuration Release \
    -archivePath "$ARCHIVE_PATH" \
    -destination "generic/platform=macOS" \
    archive \
    CODE_SIGN_STYLE=Manual \
    CODE_SIGN_IDENTITY="Apple Distribution" \
    DEVELOPMENT_TEAM="$TEAM_ID" \
    PROVISIONING_PROFILE_SPECIFIER="Sparkamp Mac App Store" \
    CODE_SIGN_ENTITLEMENTS="$ENTITLEMENTS" \
    > "$ARCHIVE_LOG" 2>&1
rc=$?
set -e
if [ $rc -ne 0 ] || [ ! -d "$ARCHIVE_PATH" ]; then
    echo "ERROR: archive failed. Last 40 lines:" >&2
    tail -40 "$ARCHIVE_LOG" >&2
    exit 1
fi
echo "  archived to $ARCHIVE_PATH"

say "[3/4] Exporting the .pkg"
rm -rf "$EXPORT_DIR"
EXPORT_LOG="$(mktemp -t sparkamp-appstore-export)"
set +e
xcodebuild \
    -exportArchive \
    -archivePath "$ARCHIVE_PATH" \
    -exportPath "$EXPORT_DIR" \
    -exportOptionsPlist "$EXPORT_PLIST" \
    > "$EXPORT_LOG" 2>&1
rc=$?
set -e
if [ $rc -ne 0 ]; then
    echo "ERROR: export failed. Last 40 lines:" >&2
    tail -40 "$EXPORT_LOG" >&2
    exit 1
fi

say "[4/4] Verifying what came out"
APP="$ARCHIVE_PATH/Products/Applications/${BUNDLE_NAME}.app"
[ -d "$APP" ] || APP="$(find "$ARCHIVE_PATH/Products/Applications" -maxdepth 1 -name '*.app' | head -1)"
fail=0

# The sandbox is the whole point. An unsandboxed build is rejected, and the
# rejection does not say why in these terms.
if codesign -d --entitlements - --xml "$APP" 2>/dev/null \
    | plutil -convert xml1 -o - - 2>/dev/null \
    | grep -q "com.apple.security.app-sandbox"; then
  echo "  sandboxed: yes"
else
  echo "  sandboxed: NO — the entitlements did not make it into the signature" >&2
  fail=1
fi

# This whole effort was about not shipping GStreamer. Check, do not assume.
if find "$APP" -name '*gst*' -o -name 'liborc*' | grep -q .; then
  echo "  GStreamer in the bundle: YES — this must not ship" >&2
  find "$APP" -name '*gst*' -o -name 'liborc*' | head -5 >&2
  fail=1
else
  echo "  GStreamer in the bundle: none"
fi

actual_id="$(defaults read "$APP/Contents/Info" CFBundleIdentifier 2>/dev/null || echo '?')"
if [ "$actual_id" = "$BUNDLE_ID" ]; then
  echo "  bundle id: $actual_id"
else
  echo "  bundle id: $actual_id (expected $BUNDLE_ID)" >&2
  fail=1
fi

if ! codesign --verify --deep --strict "$APP" 2>/dev/null; then
  echo "  signature: does not verify" >&2
  fail=1
else
  echo "  signature: verifies"
fi

[ $fail -eq 0 ] || { echo "" >&2; echo "Refusing to hand over a package that fails its own checks." >&2; exit 1; }

mkdir -p "$DIST_DIR"
PKG="$(find "$EXPORT_DIR" -name '*.pkg' | head -1)"
if [ -n "$PKG" ]; then
  cp "$PKG" "$DIST_DIR/${BUNDLE_NAME}-${VERSION}.pkg"
  say "Done: dist/${BUNDLE_NAME}-${VERSION}.pkg"
  cat <<MSG

To upload, either:
  • Transporter.app (App Store Connect → drag the .pkg in), or
  • xcrun altool --upload-app -f "dist/${BUNDLE_NAME}-${VERSION}.pkg" \\
        -t macos -u <your-apple-id> -p <app-specific-password>

An app-specific password is made at https://appleid.apple.com — never your
account password.
MSG
else
  say "Export produced no .pkg. Contents of $EXPORT_DIR:"
  ls -la "$EXPORT_DIR"
  exit 1
fi
