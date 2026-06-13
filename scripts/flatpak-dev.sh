#!/usr/bin/env bash
# Build and run Sparkamp as a Flatpak on the host — outside the distrobox.
#
# Why this exists
# ───────────────
# The Arch distrobox (dev-box) is great for fast `cargo build && cargo test`,
# but it is NOT the shipped environment. Device support depends on the Flatpak
# sandbox: the udisks2 system-bus permission, the document portal, and the
# /.flatpak-info diagnostics. Those only behave correctly when Sparkamp runs as
# the real Flatpak on the host. This script builds that Flatpak and runs it on
# Bazzite (or any host) with your physically-plugged-in USB sticks / SD cards
# visible exactly as a user would see them.
#
# Immutable-host friendly: builds with org.flatpak.Builder (itself a Flatpak),
# so no -devel packages or rpm-ostree layering are required.
#
# Usage
# ─────
#   scripts/flatpak-dev.sh           # build + install (--user) + run
#   scripts/flatpak-dev.sh -r        # run only, skip the build
#   scripts/flatpak-dev.sh -b        # build + install, do not run
#   scripts/flatpak-dev.sh -c        # clean build output and caches, then build+run

set -euo pipefail

cd "$(dirname "$0")/.."

APP_ID="dev.sparkamp.Sparkamp"
MANIFEST="$APP_ID.yml"
BUILD_DIR="build-dir"

DO_BUILD=1
DO_RUN=1
DO_CLEAN=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    -r|--run)   DO_BUILD=0 ;;
    -b|--build) DO_RUN=0 ;;
    -c|--clean) DO_CLEAN=1 ;;
    -h|--help)  grep '^#' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 1 ;;
  esac
  shift
done

if ! command -v flatpak >/dev/null 2>&1; then
  echo "error: flatpak not found. Install it first (Bazzite ships it by default)." >&2
  exit 1
fi

# org.flatpak.Builder provides flatpak-builder without host tooling.
if [[ "$DO_BUILD" == 1 ]] && ! flatpak info org.flatpak.Builder >/dev/null 2>&1; then
  echo "org.flatpak.Builder is not installed. Install it with:" >&2
  echo "  flatpak install -y flathub org.flatpak.Builder" >&2
  exit 1
fi

if [[ "$DO_CLEAN" == 1 ]]; then
  echo "Cleaning $BUILD_DIR and .flatpak-builder caches…"
  rm -rf "$BUILD_DIR" .flatpak-builder
fi

if [[ "$DO_BUILD" == 1 ]]; then
  echo "Building $APP_ID from $MANIFEST (this compiles the Rust core; first run is slow)…"
  # --user installs to the per-user Flatpak store; --install-deps-from pulls the
  # GNOME runtime/SDK + rust-stable extension if missing; --force-clean wipes the
  # previous build tree so stale objects never leak in.
  flatpak run org.flatpak.Builder \
    --force-clean --user --install --install-deps-from=flathub \
    "$BUILD_DIR" "$MANIFEST"
fi

if [[ "$DO_RUN" == 1 ]]; then
  echo "Launching $APP_ID — plug in a device to test detection."
  exec flatpak run "$APP_ID"
fi
