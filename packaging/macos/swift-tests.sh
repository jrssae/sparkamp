#!/bin/bash
# packaging/macos/swift-tests.sh
#
# Run the macOS frontend's Swift tests (frontends/SparkampMac/Tests).
#
# They cover the Swift-only code: the skin parser, the key map and its
# shortcut list, formatting, drag payloads, visualizer arithmetic, the Files
# table's columns, and the Swift half of calls into the core. The Rust core
# has its own tests (`cargo test`); these do not repeat them.
#
# macOS only, and nothing here launches Sparkamp. The test bundle compiles
# the app's sources itself (every file but the `@main` entry point) and links
# the Rust static library, so no test touches a real library, config or
# window position.
#
# Signing is overridden the way build-dmg.sh does it, so this runs on a
# machine or CI runner with no Apple Development certificate.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
DERIVED="${SPARKAMP_TEST_DERIVED_DATA:-$REPO_ROOT/target/swift-tests}"

cd "$REPO_ROOT"
cargo build --release --manifest-path frontends/macos/Cargo.toml
cp target/release/libsparkamp_macos.a frontends/SparkampMac/libsparkamp_macos.a

xcodebuild test \
    -project frontends/SparkampMac/SparkampMac.xcodeproj \
    -scheme SparkampMac \
    -destination "platform=macOS" \
    -derivedDataPath "$DERIVED" \
    CODE_SIGN_IDENTITY="-" \
    CODE_SIGNING_REQUIRED=NO \
    CODE_SIGNING_ALLOWED=NO
