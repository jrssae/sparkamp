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
