#!/bin/bash
# packaging/macos/build-sandboxed-local.sh
#
# Builds Sparkamp with the App Store sandbox entitlements, signed with the
# Apple Development certificate, and leaves it somewhere you can run it.
#
# Why this exists
# ────────────────
# The test suite runs outside the sandbox, where every permission and tool it
# needs already exists. Sparkamp has been here before: v1.3.3's release notes
# are eight failures the Flatpak sandbox surfaced that no test caught, from
# disc access to ripping to network lookups.
#
# The macOS App Sandbox will have its own list and nobody has looked. This is
# how you look, and it needs no Apple Distribution certificate and no
# submission — only the Apple Development certificate that is already here.
#
# Not for distribution. The output is signed for this machine.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
APP_NAME="SparkampMac"
BUNDLE_NAME="Sparkamp"
BUNDLE_ID="com.sparkamp.sparkampmac"
TEAM_ID="HR3P54M383"
XCODEPROJ="$REPO_ROOT/frontends/SparkampMac/$APP_NAME.xcodeproj"
ENTITLEMENTS="$SCRIPT_DIR/entitlements-appstore.plist"
BUILD_DIR="/tmp/${APP_NAME}-sandboxed"

say() { printf '\n==> %s\n' "$*"; }

say "[0/4] Checking for a signing identity"
IDENTITY="$(security find-identity -v -p codesigning 2>/dev/null \
    | grep "Apple Development" | head -1 | sed -E 's/.*"(.*)"/\1/')"
if [ -z "$IDENTITY" ]; then
    cat >&2 <<'MSG'
No "Apple Development" certificate on this machine.

    Xcode → Settings → Accounts → <your Apple ID> → Manage Certificates
          → the + button → Apple Development

A sandboxed app has to be signed to run at all. Ad-hoc signing will not do:
the sandbox reads its entitlements out of the signature.
MSG
    exit 1
fi
echo "  $IDENTITY"

say "[1/4] Building the Rust static library"
cd "$REPO_ROOT"
cargo build --release --manifest-path frontends/macos/Cargo.toml
cp target/release/libsparkamp_macos.a frontends/SparkampMac/libsparkamp_macos.a

say "[2/4] Building the app, sandboxed"
rm -rf "$BUILD_DIR"
BUILD_LOG="$(mktemp -t sparkamp-sandboxed)"
set +e
xcodebuild \
    -project "$XCODEPROJ" \
    -scheme "$APP_NAME" \
    -configuration Release \
    -derivedDataPath "$BUILD_DIR" \
    -destination "generic/platform=macOS" \
    build \
    CODE_SIGN_STYLE=Automatic \
    DEVELOPMENT_TEAM="$TEAM_ID" \
    CODE_SIGN_ENTITLEMENTS="$ENTITLEMENTS" \
    -allowProvisioningUpdates \
    > "$BUILD_LOG" 2>&1
rc=$?
set -e
APP="$(find "$BUILD_DIR/Build/Products" -maxdepth 2 -name '*.app' 2>/dev/null | head -1)"
if [ $rc -ne 0 ] || [ -z "$APP" ]; then
    echo "ERROR: build failed. Last 40 lines:" >&2
    tail -40 "$BUILD_LOG" >&2
    exit 1
fi
echo "  $APP"

say "[3/4] Confirming the sandbox is actually on"
# Out of the signature, not out of the plist that was passed in. Those are
# different things and only one of them constrains the running process.
ENTS="$(codesign -d --entitlements - --xml "$APP" 2>/dev/null | plutil -convert xml1 -o - - 2>/dev/null)"
if echo "$ENTS" | grep -q "com.apple.security.app-sandbox"; then
    echo "  sandboxed: yes"
else
    echo "  sandboxed: NO. The entitlements did not reach the signature." >&2
    exit 1
fi
echo "$ENTS" | grep -oE "com\.apple\.security\.[a-z.-]+" | sed 's/^/    /' | sort -u

say "[4/4] Ready"
cat <<MSG

  open "$APP"

The container it will use, which starts empty:

  ~/Library/Containers/$BUNDLE_ID/Data

What to watch for, in rough order of likelihood:

  • The library appears empty. Expected on a first run: the container has no
    database, and the folders a DMG install added are unreachable until the
    user re-picks them. This is the migration that has not been built.
  • A disc reads as present with no CD-TEXT. That is the open question, whether
    the sandbox permits opening /dev/rdiskN. Run
    cargo test --lib live_cdtext_absence_is_quiet -- --ignored
    against the same disc outside the sandbox, to tell a denial from a disc
    that simply has none.
  • Anything failing silently. Sandbox denials are quiet by design. Watch them:

      log stream --predicate 'sender == "Sandbox"' --style compact

MSG
