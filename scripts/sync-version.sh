#!/usr/bin/env bash
# Sync version across all package manifests.
#
# Single source of truth: Cargo.toml `version = "..."`.
# Propagates to:
#   - frontends/SparkampMac/SparkampMac.xcodeproj/project.pbxproj (MARKETING_VERSION, both targets)
# Verifies (does not edit):
#   - packaging/dev.sparkamp.Sparkamp.metainfo.xml has a <release version="$VERSION"> entry
#
# Run before tagging a release. CI builds (build-dmg.sh, flatpak) read these files
# directly, so any drift produces wrongly-named release assets.

set -euo pipefail

cd "$(dirname "$0")/.."

CARGO_TOML="Cargo.toml"
PBXPROJ="frontends/SparkampMac/SparkampMac.xcodeproj/project.pbxproj"
METAINFO="packaging/dev.sparkamp.Sparkamp.metainfo.xml"

VERSION="$(grep -E '^version = "' "$CARGO_TOML" | head -1 | sed -E 's/^version = "([^"]+)".*/\1/')"

if [[ -z "$VERSION" ]]; then
  echo "error: could not parse version from $CARGO_TOML" >&2
  exit 1
fi

echo "Cargo.toml version: $VERSION"

# Xcode MARKETING_VERSION (both Debug + Release configurations)
if ! grep -q "MARKETING_VERSION = $VERSION;" "$PBXPROJ"; then
  echo "Updating $PBXPROJ MARKETING_VERSION → $VERSION"
  sed -i.bak -E "s/MARKETING_VERSION = [0-9]+\.[0-9]+\.[0-9]+;/MARKETING_VERSION = $VERSION;/g" "$PBXPROJ"
  rm -f "${PBXPROJ}.bak"
else
  echo "$PBXPROJ already at $VERSION"
fi

# AppStream metainfo (must be manually authored with release notes; sync only verifies)
if ! grep -q "<release version=\"$VERSION\"" "$METAINFO"; then
  echo "error: $METAINFO has no <release version=\"$VERSION\"> entry" >&2
  echo "       Add release notes under <releases> before tagging." >&2
  exit 1
fi

echo "All version sources synced at $VERSION"
