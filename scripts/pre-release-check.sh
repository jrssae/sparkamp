#!/usr/bin/env bash
# Pre-release readiness check. Run before `git tag`.
#
# Catches the two ways a release goes wrong:
#   1. Forgetting to bump — Cargo.toml still holds the already-released
#      version, so a new tag would ship an unchanged build.
#   2. Version drift — pbxproj / metainfo not matching Cargo.toml (the bug
#      that mislabeled the v1.0.2 and v1.1.0 DMGs).
#
# Usage:
#   scripts/pre-release-check.sh            # infer from Cargo.toml
#   scripts/pre-release-check.sh 1.0.4      # also assert the intended version
#
# Pass the intended version explicitly when cutting a release. With no
# argument the script can only check that Cargo.toml is newer than the last
# tag; if you forgot to bump it fails and tells you to state the version, so
# a release is never tagged at a stale or unconfirmed number.

set -euo pipefail

cd "$(dirname "$0")/.."

CARGO_TOML="Cargo.toml"
VERSION="$(grep -E '^version = "' "$CARGO_TOML" | head -1 | sed -E 's/^version = "([^"]+)".*/\1/')"
EXPECTED="${1:-}"

if [[ -z "$VERSION" ]]; then
  echo "error: could not parse version from $CARGO_TOML" >&2
  exit 1
fi

# 1. If an intended version was given, Cargo.toml must already match it.
if [[ -n "$EXPECTED" && "$EXPECTED" != "$VERSION" ]]; then
  echo "error: requested release $EXPECTED but Cargo.toml is at $VERSION." >&2
  echo "       Bump Cargo.toml to $EXPECTED and add a matching metainfo <release> entry first." >&2
  exit 1
fi

# 2. The version must be strictly newer than the latest released tag.
#    Equal-or-lower means a forgotten bump — refuse and ask for the version.
LATEST_TAG="$(git tag --list 'v*' | sed 's/^v//' | sort -V | tail -1)"
if [[ -n "$LATEST_TAG" ]]; then
  HIGHEST="$(printf '%s\n%s\n' "$VERSION" "$LATEST_TAG" | sort -V | tail -1)"
  if [[ "$VERSION" == "$LATEST_TAG" || "$HIGHEST" != "$VERSION" ]]; then
    echo "error: Cargo.toml version $VERSION is not newer than the last release v$LATEST_TAG." >&2
    echo "       Did you forget to bump the version? State the intended version, e.g.:" >&2
    echo "         scripts/pre-release-check.sh 1.0.4" >&2
    echo "       then bump Cargo.toml to it and add a metainfo <release> entry." >&2
    exit 1
  fi
fi

# 3. Propagate to pbxproj and verify the metainfo release entry exists.
bash scripts/sync-version.sh

echo "Release readiness OK — ready to tag v$VERSION"
