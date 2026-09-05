#!/usr/bin/env bash
# Generate one short tone per audio container Sparkamp supports.
#
# Why these exist
# ---------------
# Tag behaviour differs per container, and the differences are not guessable:
# a FLAC has no place for ID3's WXXX, a WAV can carry two tags at once, and
# lofty maps the same field to different keys per format. Testing that needs a
# real file of each type, so here they are, generated rather than committed by
# hand so anyone can reproduce or extend the set.
#
# They are also real decodable audio rather than bare headers, which is what
# makes them usable for rip and burn tests as well as tagging.
#
# Usage
# -----
#   tools/make-test-tones.sh                    # tagging fixtures, into tests/fixtures
#
# The default second is not arbitrary: `avg_bitrate_kbps` refuses anything at
# or under half a second, so a shorter tone can never exercise bitrate and the
# column silently reads empty in a test that looks like it covered it.
#   tools/make-test-tones.sh -d 5 -r 44100 -c 2 -o /tmp/burn
#       Red Book shaped and long enough for a real burn. A CD track has a
#       four-second minimum, so -d 5 is the smallest that will actually write.
#       Do not commit those: five seconds of 44.1 kHz stereo is megabytes.
#
# Two containers have no encoder here and are skipped: Monkey's Audio (ape)
# needs `mac`, Musepack (mpc) needs `mpcenc`. Neither ships with ffmpeg.

set -euo pipefail

DURATION=1
RATE=8000
CHANNELS=1
OUT="$(cd "$(dirname "$0")/.." && pwd)/tests/fixtures"

while getopts "d:r:c:o:h" opt; do
  case "$opt" in
    d) DURATION="$OPTARG" ;;
    r) RATE="$OPTARG" ;;
    c) CHANNELS="$OPTARG" ;;
    o) OUT="$OPTARG" ;;
    h) grep '^#' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "try -h" >&2; exit 1 ;;
  esac
done

# `stat` is not portable: GNU takes -c%s, BSD takes -f%z. This printed an
# error per format on macOS and reported every size as empty.
filesize() {
  stat -f%z "$1" 2>/dev/null || stat -c%s "$1" 2>/dev/null || echo "?"
}

command -v ffmpeg >/dev/null || { echo "error: ffmpeg not found" >&2; exit 1; }

# Vorbis has two encoders and builds disagree about which they ship. Homebrew's
# ffmpeg carries the native `vorbis` and not `libvorbis`, so naming libvorbis
# outright meant tone.ogg was simply missing from the set on macOS, quietly,
# because the loop only warns per format.
if ffmpeg -hide_banner -encoders 2>/dev/null | grep -q ' libvorbis '; then
  VORBIS=libvorbis
else
  VORBIS=vorbis
fi
mkdir -p "$OUT"

src="$(mktemp -d)/src.wav"
trap 'rm -rf "$(dirname "$src")"' EXIT
ffmpeg -v error -f lavfi -i "sine=frequency=440:duration=$DURATION" \
  -ac "$CHANNELS" -ar "$RATE" "$src" -y

# ext:codec pairs. The extension decides the container; the codec is named
# explicitly so ffmpeg never picks a different default on someone else's build.
for spec in \
  "mp3:libmp3lame" \
  "flac:flac" \
  "ogg:$VORBIS" \
  "opus:libopus" \
  "wav:pcm_s16le" \
  "aac:aac" \
  "m4a:aac" \
  "wma:wmav2" \
  "tta:tta" \
  "wv:wavpack" \
  "aiff:pcm_s16be"
do
  ext="${spec%%:*}"
  codec="${spec#*:}"
  # ffmpeg calls its own native Vorbis encoder experimental and refuses it
  # without this. The flag has to sit with the output options: placed before
  # -i it applies to the input and the encode still fails.
  strict=""
  [ "$codec" = "vorbis" ] && strict="-strict -2"
  if ffmpeg -v error -i "$src" -c:a "$codec" $strict "$OUT/tone.$ext" -y 2>/dev/null \
     && [ -s "$OUT/tone.$ext" ]; then
    printf '  %-5s %8s bytes\n' "$ext" "$(filesize "$OUT/tone.$ext")"
  else
    # ffmpeg leaves a zero-byte file behind when an encoder refuses, and a
    # zero-byte tone.ogg is worse than no tone.ogg: it looks like a fixture.
    rm -f "$OUT/tone.$ext"
    printf '  %-5s SKIPPED (%s encoder unavailable)\n' "$ext" "$codec"
  fi
done

echo "Wrote tones to $OUT"
