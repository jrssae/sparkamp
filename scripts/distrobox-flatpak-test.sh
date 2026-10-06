#!/usr/bin/env bash
# Install and smoke-test the Sparkamp Flatpak on Ubuntu, Arch and Fedora,
# one distrobox per distro.
#
# Why this exists
# ───────────────
# The Flatpak brings its own GNOME runtime, so the app's libraries are the
# same everywhere. What still differs per distro is everything around it: the
# flatpak and bubblewrap versions that install and sandbox it, and each
# distro's sudo and packaging quirks. This script installs one bundle into a
# box per distro and checks that it installs, reports the right version, and
# that the GUI and the TUI start and stay up without crashing, and that the GUI
# claims its MPRIS bus name through the sandbox's D-Bus filter.
#
# What it cannot catch: a distrobox shares the host's kernel, compositor,
# PipeWire, portals, udisks and GPU driver. Bugs that depend on the desktop
# (COSMIC, portals, SELinux/AppArmor, optical drives) need a VM or real
# hardware; scripts/flatpak-dev.sh covers device testing on the host.
#
# Each box has its own home directory, so nothing here touches your real
# ~/.config/sparkamp or your host Flatpak installation. The GUI runs on a
# headless Wayland compositor, so no window appears on your desktop, and
# each run starts the app from an empty home with a private session bus.
#
# Usage
# ─────
#   scripts/distrobox-flatpak-test.sh --build             # build this checkout, then test it
#   scripts/distrobox-flatpak-test.sh --build ~/src/sparkamp
#   scripts/distrobox-flatpak-test.sh --bundle Sparkamp-<sha>.flatpak
#   scripts/distrobox-flatpak-test.sh --release           # latest GitHub release
#   scripts/distrobox-flatpak-test.sh --release v1.4.1
#
#   --build [DIR]       Build a bundle from a source tree (default: the checkout
#                       holding this script) and test it. Runs `cargo vendor` in
#                       that tree first, as packaging/README.md requires.
#   --bundle FILE       Test a .flatpak you already have: your own build or a
#                       CI artifact.
#   --release [TAG]     Download a release's .flatpak (default: the latest).
#   --distro LIST       Comma-separated subset, e.g. ubuntu,fedora (default: all).
#   --gui-seconds N     How long the GUI and TUI must stay up (default: 20).
#   --setup-only        Create and update the boxes without testing anything.
#   -h, --help          Show this help.
#
# Everything is kept under $SPARKAMP_TEST_DIR (default
# ~/.local/share/sparkamp-distrobox-test): box homes, bundles, the build cache,
# and a results folder per run with logs and a GUI screenshot per distro.
# Remove a box with `distrobox rm sparkamp-test-<distro>`.

set -euo pipefail

APP_ID="dev.sparkamp.Sparkamp"
MPRIS_NAME="org.mpris.MediaPlayer2.sparkamp"
MANIFEST="$APP_ID.yml"
FLATHUB_REPO="https://dl.flathub.org/repo/flathub.flatpakrepo"
GITHUB_REPO="${SPARKAMP_GITHUB_REPO:-jrssae/sparkamp}"
BOX_PREFIX="sparkamp-test"

# One box per line: name, package manager, image. Toolbx images are used
# because distrobox does not have to install its own dependencies into them.
# Changing an image here does not touch an existing box; `distrobox rm` it.
DISTROS=(
  "ubuntu apt    quay.io/toolbx/ubuntu-toolbox:26.04"
  "arch   pacman quay.io/toolbx/arch-toolbox:latest"
  "fedora dnf    registry.fedoraproject.org/fedora-toolbox:44"
)
# Builds get a box of their own so the test boxes never have the SDK, the same
# as a user's machine.
BUILD_BOX="$BOX_PREFIX-build"
BUILD_IMAGE="registry.fedoraproject.org/fedora-toolbox:44"
BUILD_PACKAGES="flatpak flatpak-builder cargo"

SELF="$(realpath "$0")"

die() { echo "error: $*" >&2; exit 1; }

# ── Inside a box ─────────────────────────────────────────────────────────────
# The host half of this script re-runs it inside each box as
# `__in-box <step> ...`. Everything it needs arrives as arguments, because the
# box has a different $HOME from the host.

# Commands the test boxes need, and the packages that provide them per distro.
TEST_COMMANDS="flatpak weston script dbus-run-session gdbus"
packages_for() {
  case "$1" in
    apt)    echo "flatpak weston bsdutils dbus-daemon libglib2.0-bin" ;;
    pacman) echo "flatpak weston util-linux dbus glib2" ;;
    dnf)    echo "flatpak weston util-linux-script dbus-daemon glib2" ;;
  esac
}

# `sudo -H` everywhere so root never writes into the box's home, whatever a
# distro's sudoers defaults are. A root-owned file there breaks flatpak for
# the user (see reclaim_home).
install_packages() {
  local pm="$1"; shift
  case "$pm" in
    apt)    sudo -H apt-get update -qq
            sudo -H env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "$@" ;;
    # Arch does not support partial upgrades, so installing means upgrading.
    # --overwrite replaces distrobox's host-exec link (see own_command).
    pacman) sudo -H pacman -Syu --noconfirm --needed --overwrite /usr/bin/flatpak "$@" ;;
    dnf)    sudo -H dnf install -y -q "$@" ;;
  esac
}

# When a box has no flatpak of its own, distrobox links /usr/bin/flatpak to
# distrobox-host-exec, so `flatpak` in the box quietly runs the host's. The
# tests must never do that: they would install into your real system. Boxes
# made by this script get flatpak at creation, which stops distrobox making
# the link; this check guards boxes that were made some other way.
own_command() {
  local path
  path="$(command -v "$1")" || return 1
  [[ "$(readlink -f "$path")" != */distrobox-host-exec ]]
}

require_own_flatpak() {
  own_command flatpak && return
  die "flatpak in ${CONTAINER_ID:-this box} is the host's, not the box's own. Remove the box with \`distrobox rm ${CONTAINER_ID:-<box>}\` and run this script again."
}

ensure_tools() {  # package-manager "commands" "packages"
  local cmd
  for cmd in $2; do
    if ! own_command "$cmd"; then
      # shellcheck disable=SC2086  # word splitting is the point
      install_packages "$1" $3
      return
    fi
  done
}

# A box sharing your real home would let the tests read and overwrite your own
# Sparkamp settings, and put --user Flatpak installs into your host's store.
check_home() {
  [[ "$HOME" == "$1" ]] && return
  die "${CONTAINER_ID:-this box} uses HOME=$HOME instead of $1. Remove it with \`distrobox rm ${CONTAINER_ID:-<box>}\` and run this script again."
}

# distrobox's first start runs as root with the box's HOME, and on Arch and
# Fedora it leaves a root-only ~/.cache behind. `flatpak run` keeps its cache
# there, so every launch then fails with "Permission denied". Only call this
# after check_home, so it can only ever touch the box's own home.
reclaim_home() {
  if [[ -n "$(find "$HOME" -xdev ! -user "$(id -u)" -print -quit)" ]]; then
    sudo chown -R "$(id -u):$(id -g)" "$HOME"
  fi
}

in_box_setup() {  # package-manager expected-home
  check_home "$2"
  reclaim_home
  ensure_tools "$1" "$TEST_COMMANDS" "$(packages_for "$1")"
  require_own_flatpak
  sudo -H flatpak remote-add --system --if-not-exists flathub "$FLATHUB_REPO"
  # Keep the runtime current, as it would be on a user's machine.
  sudo -H flatpak update --system -y --noninteractive
}

in_box_build() {  # source-dir work-dir out-bundle expected-home
  local src="$1" dir="$2" out="$3"
  check_home "$4"
  reclaim_home
  ensure_tools dnf "flatpak flatpak-builder cargo" "$BUILD_PACKAGES"
  require_own_flatpak
  # Mirrors .github/workflows/build.yml, so a bundle built here matches the
  # one CI would produce from the same tree.
  flatpak remote-add --user --if-not-exists flathub "$FLATHUB_REPO"
  (cd "$src" && cargo vendor --quiet >/dev/null)
  # The build and state dirs live outside the source tree because the
  # manifest's `type: dir` source would otherwise copy them into the build.
  # rofiles-fuse cannot mount in a rootless container ("fusermount3: failed
  # to access mountpoint"); it only guards the build cache against writes.
  flatpak-builder --user --force-clean --install-deps-from=flathub --disable-rofiles-fuse \
    --state-dir="$dir/state" --repo="$dir/repo" "$dir/build-dir" "$src/$MANIFEST"
  flatpak build-bundle "$dir/repo" "$out" "$APP_ID" --runtime-repo="$FLATHUB_REPO"
}

# Each check appends key=value lines to $out/result for the host to summarise.
in_box_test() {  # bundle out-dir seconds expected-version box-name
  local bundle="$1" out="$2" secs="$3" expected="$4" name="$5"
  local home="$out/home" result="$out/result" rc got
  require_own_flatpak
  mkdir -p "$home"
  : >"$result"

  # shellcheck disable=SC1091
  . /etc/os-release
  {
    # Arch's VERSION_ID is a snapshot stamp; "rolling" says more.
    if [[ "${BUILD_ID:-}" == rolling ]]; then echo "os=$ID rolling"; else echo "os=$ID ${VERSION_ID:-}"; fi
    echo "flatpak=$(flatpak --version | awk '{print $2}')"
    echo "bwrap=$(bwrap --version 2>/dev/null | awk '{print $2}')"
  } >>"$result"

  # Uninstall first so a rebuilt bundle with the same version is not skipped.
  # The log is written as the user on purpose, hence SC2024.
  : >"$out/install.log"
  if flatpak info --system "$APP_ID" >/dev/null 2>&1; then
    # shellcheck disable=SC2024
    sudo -H flatpak uninstall --system -y --noninteractive "$APP_ID" >>"$out/install.log" 2>&1 || true
  fi
  # shellcheck disable=SC2024
  if ! sudo -H flatpak install --system -y --noninteractive --bundle "$bundle" >>"$out/install.log" 2>&1; then
    echo "install=FAIL" >>"$result"
    return
  fi
  echo "install=PASS" >>"$result"
  flatpak info --system "$APP_ID" >"$out/info.txt" 2>&1 || true

  # Every launch gets this run's empty home. The XDG variables matter as much
  # as HOME: distrobox points them at the box's home, and the manifest's
  # xdg-config/sparkamp grants follow them, so settings would otherwise carry
  # over from one run to the next.
  local -a run_env=(env HOME="$home" XDG_CONFIG_HOME="$home/.config"
    XDG_DATA_HOME="$home/.local/share" XDG_CACHE_HOME="$home/.cache"
    XDG_STATE_HOME="$home/.local/state")

  # A private session bus keeps the app off your desktop's bus, where it would
  # otherwise register as a media player next to anything you are running.
  rc=0
  "${run_env[@]}" dbus-run-session -- timeout 60 flatpak run "$APP_ID" --version >"$out/version.log" 2>&1 || rc=$?
  got="$(sed -n 's/^sparkamp //p' "$out/version.log" | head -n1)"
  if [[ $rc -eq 0 && -n "$got" && ( -z "$expected" || "$got" == "$expected" ) ]]; then
    echo "version=$got" >>"$result"
  else
    echo "version=FAIL${got:+ ($got)}" >>"$result"
  fi

  # GUI: a headless weston stands in for the desktop. Pixman rendering keeps
  # it off the GPU and makes the screenshot work.
  local sock="wayland-$name-$$" wpid apid
  weston --backend=headless --renderer=pixman --width=1280 --height=800 \
    --idle-time=0 --debug --socket="$sock" >"$out/weston.log" 2>&1 &
  wpid=$!
  for _ in $(seq 50); do
    [[ -S "$XDG_RUNTIME_DIR/$sock" ]] && break
    sleep 0.2
  done
  if [[ -S "$XDG_RUNTIME_DIR/$sock" ]]; then
    rc=0
    # Halfway through, ask the app's private bus whether it holds its MPRIS
    # name; the probe has to share the dbus-run-session to see that bus.
    "${run_env[@]}" WAYLAND_DISPLAY="$sock" DISPLAY="" dbus-run-session -- bash -c '
      timeout -k 5 "$1" flatpak run --nosocket=x11 "$2" >"$3/gui.log" 2>&1 &
      app=$!
      sleep $(( $1 / 2 ))
      gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus \
        --method org.freedesktop.DBus.NameHasOwner "$4" >"$3/mpris.txt" 2>&1
      wait "$app"' _ "$secs" "$APP_ID" "$out" "$MPRIS_NAME" &
    apid=$!
    sleep $(( secs > 8 ? secs - 4 : secs / 2 ))
    (cd "$out" && WAYLAND_DISPLAY="$sock" weston-screenshooter) >/dev/null 2>&1 || true
    wait "$apid" || rc=$?
    local shot
    for shot in "$out"/wayland-screenshot-*.png; do
      [[ -e "$shot" ]] && mv -f "$shot" "$out/gui.png"
    done
    echo "gui=$(stayed_up "$rc" "$out/gui.log")" >>"$result"
    # Without the manifest's --own-name grant the sandbox refuses the name,
    # and media keys and the desktop's media controls stop working with no
    # crash to notice. Ask the bus rather than grep the log: the app's "could
    # not own bus name" line also appears at shutdown under flatpak 1.18, when
    # the proxy closes the bus before the app exits.
    if grep -qF '(true,)' "$out/mpris.txt" 2>/dev/null; then
      echo "mpris=PASS" >>"$result"
    else
      echo "mpris=FAIL (name not owned)" >>"$result"
    fi
  else
    echo "gui=FAIL (weston did not start)" >>"$result"
  fi
  kill "$wpid" 2>/dev/null || true
  wait "$wpid" 2>/dev/null || true

  # TUI: `script` gives it a terminal. --foreground keeps it in the
  # terminal's foreground group; without that it is stopped on its first read.
  rc=0
  "${run_env[@]}" TERM=xterm-256color script -qefc \
    "stty rows 40 cols 120; exec dbus-run-session -- timeout --foreground -k 5 $secs flatpak run $APP_ID --tui" \
    "$out/tui.log" </dev/null >/dev/null 2>&1 || rc=$?
  echo "tui=$(stayed_up "$rc" "$out/tui.log")" >>"$result"
}

# timeout exits 124 when it had to stop the app, which is the pass: the app
# was still running. 137 means it also needed a SIGKILL, which is not a crash.
# Any other exit means the app quit or crashed on its own.
stayed_up() {  # exit-code log
  if [[ $1 -ne 124 && $1 -ne 137 ]]; then
    echo "FAIL (exited $1)"
  elif grep -qE 'panicked at|Segmentation fault|core dumped' "$2"; then
    echo "FAIL (crash in log)"
  else
    echo "PASS"
  fi
}

if [[ "${1:-}" == "__in-box" ]]; then
  step="$2"; shift 2
  case "$step" in
    setup) in_box_setup "$@" ;;
    build) in_box_build "$@" ;;
    test)  in_box_test "$@" ;;
    *)     die "unknown in-box step: $step" ;;
  esac
  exit
fi

# ── On the host ──────────────────────────────────────────────────────────────

usage() { awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$SELF"; }

SOURCE=""
SOURCE_ARG=""
ONLY=""
# 20, not 15: the screenshot is taken 4 s before the end, and on Ubuntu 26.04's
# weston 14 the window maps 8-12 s after launch, while GTK falls back from a
# Vulkan surface the headless compositor loses. At 15 the screenshot caught an
# empty desktop about half the time.
GUI_SECONDS=20
SETUP_ONLY=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --build|--release)
      SOURCE="${1#--}"
      if [[ -n "${2:-}" && "$2" != -* ]]; then SOURCE_ARG="$2"; shift; fi ;;
    --bundle)      SOURCE=bundle; SOURCE_ARG="${2:?--bundle needs a file}"; shift ;;
    --distro)      ONLY="${2:?--distro needs a list}"; shift ;;
    --gui-seconds) GUI_SECONDS="${2:?--gui-seconds needs a number}"; shift ;;
    --setup-only)  SETUP_ONLY=1 ;;
    -h|--help)     usage; exit 0 ;;
    *)             die "unknown option: $1 (see --help)" ;;
  esac
  shift
done

[[ -n "$SOURCE" || $SETUP_ONLY == 1 ]] || die "say what to test: --build, --bundle or --release (see --help)"
[[ "$GUI_SECONDS" =~ ^[0-9]+$ && "$GUI_SECONDS" -ge 5 ]] || die "--gui-seconds must be a whole number of at least 5"
# distrobox cannot create boxes from inside one, e.g. from a dev box.
[[ -e /run/.containerenv || -e /.dockerenv ]] && die "run this on the host, not inside a container"
command -v distrobox >/dev/null 2>&1 || die "distrobox is not installed"
MANAGER="${DBX_CONTAINER_MANAGER:-$(command -v podman >/dev/null 2>&1 && echo podman || echo docker)}"

SELECTED=()
for entry in "${DISTROS[@]}"; do
  read -r name _ _ <<<"$entry"
  if [[ -z "$ONLY" || ",$ONLY," == *",$name,"* ]]; then SELECTED+=("$entry"); fi
done
[[ ${#SELECTED[@]} -gt 0 ]] || die "--distro matched nothing; known: $(printf '%s ' "${DISTROS[@]%% *}")"

WORK="$(realpath -m "${SPARKAMP_TEST_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/sparkamp-distrobox-test}")"
RUN_DIR="$WORK/results/$(date +%Y%m%d-%H%M%S)"
mkdir -p "$WORK/homes" "$WORK/bundles" "$WORK/build" "$RUN_DIR"
ln -sfn "$RUN_DIR" "$WORK/results/latest"

ensure_box() {  # box image home "packages"
  mkdir -p "$3"
  if ! "$MANAGER" container inspect "$1" >/dev/null 2>&1; then
    # Installing the packages during creation, rather than on first use, is
    # what keeps distrobox from linking flatpak to the host's (own_command).
    distrobox create --yes --no-entry --name "$1" --image "$2" --home "$3" \
      --additional-packages "$4"
    return
  fi
  local image
  image="$("$MANAGER" container inspect -f '{{.Config.Image}}' "$1")"
  [[ "$image" == "$2" ]] || echo "warning: $1 runs $image, not $2. Remove it with \`distrobox rm $1\` to switch."
}

fetch_release() {
  local api="https://api.github.com/repos/$GITHUB_REPO/releases/latest" json url tag
  if [[ -n "$SOURCE_ARG" ]]; then
    tag="$SOURCE_ARG"
    [[ "$tag" == v* ]] || tag="v$tag"
    api="https://api.github.com/repos/$GITHUB_REPO/releases/tags/$tag"
  fi
  json="$(curl -fsSL "$api")" || die "could not read $api"
  tag="$(grep -oE '"tag_name": *"[^"]+"' <<<"$json" | head -n1 | sed -E 's/.*"([^"]+)"$/\1/')"
  url="$(grep -oE '"browser_download_url": *"[^"]+\.flatpak"' <<<"$json" | head -n1 | grep -oE 'https://[^"]+')" ||
    die "release $tag has no .flatpak asset"
  BUNDLE="$WORK/bundles/$(basename "$url")"
  EXPECTED="${tag#v}"
  if [[ ! -s "$BUNDLE" ]]; then
    echo "Downloading $url"
    curl -fL --progress-bar -o "$BUNDLE.part" "$url"
    mv -f "$BUNDLE.part" "$BUNDLE"
  fi
}

build_bundle() {
  local src label log="$RUN_DIR/build.log"
  src="$(realpath "${SOURCE_ARG:-$(dirname "$SELF")/..}")"
  [[ -f "$src/$MANIFEST" ]] || die "$src has no $MANIFEST"
  label="$(git -C "$src" describe --tags --always --dirty 2>/dev/null || date +%Y%m%d-%H%M%S)"
  BUNDLE="$WORK/bundles/Sparkamp-local-$label.flatpak"
  EXPECTED="$(grep -E '^version = "' "$src/Cargo.toml" | head -1 | sed -E 's/^version = "([^"]+)".*/\1/')"
  echo "Building $src ($label) in $BUILD_BOX. The first build is slow; log: $log"
  { ensure_box "$BUILD_BOX" "$BUILD_IMAGE" "$WORK/homes/build" "$BUILD_PACKAGES" &&
    distrobox enter "$BUILD_BOX" -- bash "$SELF" __in-box build \
      "$src" "$WORK/build" "$BUNDLE" "$WORK/homes/build" </dev/null
  } >"$log" 2>&1 || die "build failed; see $log"
}

run_box() {  # name package-manager image
  local name="$1" box="$BOX_PREFIX-$1" home="$WORK/homes/$1" out="$RUN_DIR/$1"
  mkdir -p "$out"
  if ! { ensure_box "$box" "$3" "$home" "$(packages_for "$2")" &&
         distrobox enter "$box" -- bash "$SELF" __in-box setup "$2" "$home" </dev/null
       } >"$out/setup.log" 2>&1; then
    echo "setup=FAIL" >"$out/result"
  elif [[ $SETUP_ONLY == 1 ]]; then
    echo "setup=PASS" >"$out/result"
  else
    distrobox enter "$box" -- bash "$SELF" __in-box test \
      "$BUNDLE" "$out" "$GUI_SECONDS" "$EXPECTED" "$box" </dev/null >"$out/test.log" 2>&1 || true
  fi
  echo "  $name finished"
}

BUNDLE=""
EXPECTED=""
case "$SOURCE" in
  release) fetch_release ;;
  build)   build_bundle ;;
  bundle)  BUNDLE="$(realpath "$SOURCE_ARG")"; [[ -f "$BUNDLE" ]] || die "no such bundle: $SOURCE_ARG" ;;
esac

if [[ $SETUP_ONLY == 1 ]]; then
  echo "Setting up ${#SELECTED[@]} box(es); logs in $RUN_DIR"
else
  echo "Testing $(basename "$BUNDLE")${EXPECTED:+ (expecting $EXPECTED)}; logs in $RUN_DIR"
fi
for entry in "${SELECTED[@]}"; do
  read -r name pm image <<<"$entry"
  run_box "$name" "$pm" "$image" &
done
wait

# ── Summary ──────────────────────────────────────────────────────────────────

field() { grep -m1 "^$2=" "$RUN_DIR/$1/result" 2>/dev/null | cut -d= -f2- || true; }
failed=0
summarise() {
  if [[ $SETUP_ONLY == 1 ]]; then
    printf '%-8s %s\n' DISTRO SETUP
    for entry in "${SELECTED[@]}"; do
      read -r name _ _ <<<"$entry"
      printf '%-8s %s\n' "$name" "$(field "$name" setup)"
      [[ "$(field "$name" setup)" == PASS ]] || failed=1
    done
  else
    printf '%-8s %-14s %-8s %-8s %-8s %-16s %-20s %-20s %s\n' DISTRO OS FLATPAK BWRAP INSTALL VERSION GUI TUI MPRIS
    for entry in "${SELECTED[@]}"; do
      read -r name _ _ <<<"$entry"
      if [[ "$(field "$name" setup)" == FAIL ]]; then
        printf '%-8s setup FAILED, see %s\n' "$name" "$RUN_DIR/$name/setup.log"
        failed=1
        continue
      fi
      printf '%-8s %-14s %-8s %-8s %-8s %-16s %-20s %-20s %s\n' "$name" \
        "$(field "$name" os)" "$(field "$name" flatpak)" "$(field "$name" bwrap)" \
        "$(field "$name" install)" "$(field "$name" version)" "$(field "$name" gui)" \
        "$(field "$name" tui)" "$(field "$name" mpris)"
      for key in install version gui tui mpris; do
        value="$(field "$name" "$key")"
        [[ -n "$value" && "$value" != FAIL* ]] || failed=1
      done
    done
  fi
}
# Redirect rather than pipe into tee, which would run summarise in a subshell
# and lose $failed.
summarise >"$RUN_DIR/summary.txt"
echo
cat "$RUN_DIR/summary.txt"
echo
echo "Logs, per-distro screenshots and the summary: $RUN_DIR"
exit "$failed"
