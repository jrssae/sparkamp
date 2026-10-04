#!/usr/bin/env bash
# Regenerate THIRD-PARTY-NOTICES.md from the macOS app's dependency tree.
#
# The list is every crate the app links (normal dependencies, proc macros
# included; build-only tools such as cc are not shipped), grouped by the
# licence each crate declares. Run it after any dependency change:
#
#   scripts/third-party-notices.sh
#
# then review the diff. The note on symphonia below is kept by hand here,
# because a licence name alone does not explain what MPL-2.0 asks for.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MANIFEST="frontends/macos/Cargo.toml"
OUT="$REPO_ROOT/THIRD-PARTY-NOTICES.md"

cd "$REPO_ROOT"

# "<licence>\t<name> <version>" for every third-party crate, once each.
rows="$(cargo tree --manifest-path "$MANIFEST" -e normal --prefix none \
        --format '{l}|{p}' 2>/dev/null |
    sed -E 's/ \(\*\)$//; s/ \([^)]*\)$//' |
    awk -F'|' '{
        split($2, p, " ");
        name = p[1]; version = substr(p[2], 2);
        if (name == "sparkamp" || name == "sparkamp-macos") next;
        licence = ($1 == "") ? "No licence declared" : $1;
        print licence "\t" name " " version
    }' |
    LC_ALL=C sort -u)"

count="$(printf '%s\n' "$rows" | wc -l | tr -d ' ')"

{
    cat <<EOF
# Third-Party Notices

Sparkamp for macOS links the Rust crates below. Each is used under its own
licence, reproduced by its own project; this file records what they are and
is regenerated from the dependency tree rather than maintained by hand, by
\`scripts/third-party-notices.sh\`.

Generated from \`cargo tree --manifest-path $MANIFEST -e normal\`.
$count crates.

## A note on symphonia

symphonia and its codecs are **MPL-2.0**, which is a file-level copyleft: the
MPL-covered files stay under the MPL and their source stays available (it is,
upstream at <https://github.com/pdeljanov/Symphonia>), while the larger work
may be distributed under other terms. Nothing about it restricts this app's
distribution; it is listed here because its notices have to travel with it.

## By licence
EOF
    printf '%s\n' "$rows" | awk -F'\t' '
        $1 != current { current = $1; printf "\n### %s\n\n", $1 }
        { print "- " $2 }'
} > "$OUT"

echo "Wrote $OUT ($count crates)."
