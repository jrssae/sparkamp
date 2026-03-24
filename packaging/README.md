# Sparkamp Packaging

## Flatpak

### Generating cargo-sources.json

The Flatpak manifest uses an offline Cargo build.  Before the manifest can
be used, the pre-fetched Cargo dependencies must be generated:

```bash
# Install the generator (Python 3 + aiohttp required)
pip install aiohttp

# Download and run the generator
curl -O https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/master/cargo/flatpak-cargo-generator.py

# Generate the sources JSON from the lock file
python3 flatpak-cargo-generator.py ../Cargo.lock -o cargo-sources.json
```

Commit `cargo-sources.json` to the repository.  Re-run whenever
`Cargo.lock` changes (e.g. after `cargo update`).

### Building locally

```bash
# One-time: install the runtime
flatpak install org.freedesktop.Platform//23.08 \
                org.freedesktop.Sdk//23.08 \
                org.freedesktop.Sdk.Extension.rust-stable//23.08

# Build
flatpak-builder --force-clean --user build-dir ../dev.sparkamp.Sparkamp.yml

# Run directly from the build directory
flatpak-builder --run build-dir ../dev.sparkamp.Sparkamp.yml sparkamp --ui

# Bundle into a distributable .flatpak
flatpak build-bundle repo Sparkamp.flatpak dev.sparkamp.Sparkamp

# Install the bundle
flatpak install --user Sparkamp.flatpak
```

### Installing from CI artifact

Every push to `main` produces a `.flatpak` bundle as a GitHub Actions
artifact.  Download it from the workflow run and install with:

```bash
flatpak install --user Sparkamp-<sha>.flatpak
flatpak run dev.sparkamp.Sparkamp --ui
```
