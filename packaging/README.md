# Sparkamp Packaging

## Flatpak

### Vendoring the Cargo dependencies

The build is offline: `.cargo/config.toml` redirects crates-io to a local
`vendor/` directory, which is gitignored. A fresh clone has no crates at all,
so vendor before anything else.

```bash
cargo vendor
```

The `sparkamp` module in the manifest is `type: dir, path: .`, so
flatpak-builder copies the working tree including `vendor/`. That is why
`.github/workflows/build.yml` runs `cargo vendor` before it invokes
flatpak-builder, and why there is no generated sources file to keep in sync.

An earlier setup used `flatpak-cargo-generator.py` to produce a
`packaging/cargo-sources.json`. The manifest stopped referencing it, so it sat
in the repository drifting out of date against `Cargo.lock` until it was
removed. Bring it back only alongside a manifest that actually reads it.

### Building locally

Runtime versions come from the manifest, which is on GNOME 50. Installing a
different one gets you a build that does not match CI.

```bash
# One-time: install the runtime, the SDK and the Rust extension
flatpak install org.gnome.Platform//50 \
                org.gnome.Sdk//50 \
                org.freedesktop.Sdk.Extension.rust-stable//25.08

# Build
flatpak-builder --force-clean --user build-dir ../dev.sparkamp.Sparkamp.yml

# Run directly from the build directory
flatpak-builder --run build-dir ../dev.sparkamp.Sparkamp.yml sparkamp

# Bundle into a distributable .flatpak
flatpak build-bundle repo Sparkamp.flatpak dev.sparkamp.Sparkamp

# Install the bundle
flatpak install --user Sparkamp.flatpak
```

The GUI is the default invocation, so `sparkamp` takes no flag for it. `--tui`
is the only mode flag; there is no `--ui`, and clap rejects it.

### Installing from CI artifact

Every push to `main` produces a `.flatpak` bundle as a GitHub Actions
artifact. Download it from the workflow run and install with:

```bash
flatpak install --user Sparkamp-<sha>.flatpak
flatpak run dev.sparkamp.Sparkamp
```

### Testing on other distros

`scripts/distrobox-flatpak-test.sh` installs a bundle into one distrobox per
distro (Ubuntu, Arch and Fedora), then checks five things in each: the install
succeeds, `--version` reports the expected version, the GUI and the TUI both
start and stay up without crashing, and the GUI is allowed its MPRIS bus name.

```bash
scripts/distrobox-flatpak-test.sh --build                          # build this checkout, then test it
scripts/distrobox-flatpak-test.sh --bundle Sparkamp-<sha>.flatpak  # a CI artifact or your own bundle
scripts/distrobox-flatpak-test.sh --release v1.4.1                 # a published release
```

The app is identical in every box because the Flatpak brings its own GNOME
runtime. What differs per box is the flatpak and bubblewrap that install and
sandbox it, and that is what this catches. A distrobox shares the host's
kernel, compositor, audio, portals and GPU driver, so bugs that depend on the
desktop still need a VM or real hardware.

Each box has its own home directory, so the tests never touch your
`~/.config/sparkamp` or your host Flatpak installation. The GUI runs on a
headless compositor, so no window appears on your desktop. Results, logs and
one GUI screenshot per distro are written to
`~/.local/share/sparkamp-distrobox-test/results/latest/`. The script exits
non-zero if any check fails. `--help` lists the remaining options.
