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
#   tools/make-test-tones.sh -d 5 -r 44100 -c 2 -o /tmp/burn
#       Red Book shaped and long enough for a real burn. A CD track has a
#       four-second minimum, so -d 5 is the smallest that will actually write.
#       Do not commit those: five seconds of 44.1 kHz stereo is megabytes.
#
# Two containers have no encoder here and are skipped: Monkey's Audio (ape)
# needs `mac`, Musepack (mpc) needs `mpcenc`. Neither ships with ffmpeg.

set -euo pipefail

DURATION=0.15
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

command -v ffmpeg >/dev/null || { echo "error: ffmpeg not found" >&2; exit 1; }
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
  "ogg:libvorbis" \
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
  if ffmpeg -v error -i "$src" -c:a "$codec" "$OUT/tone.$ext" -y 2>/dev/null; then
    printf '  %-5s %8s bytes\n' "$ext" "$(stat -c%s "$OUT/tone.$ext")"
  else
    printf '  %-5s SKIPPED (no %s encoder)\n' "$ext" "$codec"
  fi
done

echo "Wrote tones to $OUT"
