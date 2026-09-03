# Handoff: the app-store-compatibility branch, for a Linux agent

Written on a Mac, on 3 September 2026, by the agent that did the macOS work.
You are picking this up on Linux. Read this before you build anything, because
the first command you would reach for does not work yet.

The branch is `app-store-compatibility`. It is 209 commits ahead of `main`
(branch point `e2802ead`) and has not been merged. Its purpose was to get the
macOS build through Mac App Store review, but it changed a lot of shared core
along the way, and it carries a GNOME runtime bump and a pile of GTK work that
predate the App Store push. Linux is not a bystander here.

---

## Do this first, or nothing compiles

```bash
cargo vendor
```

`.cargo/config.toml` redirects crates-io to a local `vendor/` directory, and
`vendor/` is gitignored. A fresh checkout has no crates at all. This is the
normal workflow for this repo, but the crate set grew by 33 registry crates on
this branch, so a stale `vendor/` from before the branch point will fail on
`lofty`, `metaflac`, `rustls`, `ring` and `rustfft` rather than failing
cleanly. Re-vendor rather than reusing what you have.

---

## What changed under you

### The audio engine is behind a trait now

`AudioBackend`, in [`src/engine/backend.rs`](src/engine/backend.rs), with
`GstBackend` in [`src/engine/gst.rs`](src/engine/gst.rs) and an AVFoundation
backend on the macOS side. `Player` is generic over the trait. GStreamer is
declared `cfg(not(target_os = "macos"))` and the shipped Mac app links none of
it.

Linux behaviour is meant to be identical to before. "Meant to be" is the
operative phrase: nobody has run the GTK build since the seam went in. Playback,
seeking, the equalizer and the visualizer all go through the new indirection, so
that is the first thing to regression-test, not the last.

The same shape applies to transcoding: [`src/disc/transcode/gst.rs`](src/disc/transcode/gst.rs)
against `avf.rs`, chosen at the `transcode` module.

CLAUDE.md and AGENTS.md were rewritten to describe this seam. If you are working
from a memory of the old text, re-read them.

### Tag writing now routes by container, on both platforms

This is the change most likely to surprise you, because it alters Linux
behaviour rather than merely reorganising it.

`lofty` 0.25 was added. [`src/id3_editor.rs`](src/id3_editor.rs) checks whether
a file is MPEG and uses the `id3` crate if so, and lofty otherwise. Before this,
anything that was not an MP3 silently did nothing.

Concretely:

- `write_mp3_replaygain_tags` is now `write_replaygain_tags`.
- `WriteBackOutcome::SkippedNonMp3` is now `SkippedUnsupported`, and it means
  what it says: the container genuinely cannot hold the tag, not that it is not
  an MP3.
- `apply_manual_gain_edit` in [`src/replaygain.rs`](src/replaygain.rs) is no
  longer gated to MP3, and the TUI calls it at
  [`frontends/tui/id3.rs:431`](frontends/tui/id3.rs:431).
- The GTK and TUI settings label lost its "(MP3)" suffix, because the checkbox
  now does what the label always claimed.

Test this against real FLAC and Ogg files you do not mind modifying. Writes that
used to be no-ops are now real writes.

### Disc track URIs are unified

[`src/disc/toc.rs`](src/disc/toc.rs) `track_entries()` emits
`cdda://<n>?device=<drive id>` on every platform and has no cfg gate. Linux
already produced that shape, so this is not a behaviour change for you, but the
macOS-only helpers that read mounted AIFF files were deleted, and the FFI
round-trip test now asserts `cdda://1?device=/dev/sr0`. If you see that string
in a test failure, it is the shared path, not a Linux regression.

### Erase changed signature

```rust
pub fn erase(drive: &OpticalDrive, goal: EraseGoal, progress: impl FnMut(BurnProgress))
```

`EraseGoal` is `ClearForBurn` or `MakeBlank`. On macOS it drives a quick erase
first, then verifies, then escalates to a full erase only if the quick one did
not take. On Linux the goal is ignored (`_goal`) and the body is what it always
was: `unmount_for_burn`, then `cdrskin blank=fast`. See `erase_guarded` in
[`src/disc/burn.rs`](src/disc/burn.rs), which is cfg-split.

Worth knowing why the macOS side grew a ladder. A quick erase reports success
and resets the drive's cached media descriptor whether or not it actually wrote
anything, so asking the drive "is it blank now" right afterwards always says
yes. The verification re-reads the disc through the OS instead. Linux may or may
not have the same problem through cdrskin. It has not been tested, and I would
not assume cdrskin is honest just because it is not DiscRecording.

`unmount_for_burn` in [`src/disc/mount.rs:134`](src/disc/mount.rs:134) is
Linux-only. macOS reaches the same goal through an exclusive-access guard. Do
not "fix" the asymmetry.

### The skin palette moved, which GTK renders

Dark button ramp: `#212121` / `#2e2e2e` / `#3a3a3a` became `#303030` / `#3a3a3a`
/ `#464646`. The old resting colour sat at about 1.1:1 against the background,
which reads as a flat panel rather than a button.

Light `--sp-broken-color`: `#cc5500` became `#a84600`, for contrast as body
text.

Two tests in [`src/skin.rs`](src/skin.rs) assert those exact hex strings, so a
future palette edit fails loudly rather than drifting. The palette exists in
four places (`skin.rs`, `light.css`, and two spots in the macOS `Theme.swift`).
Changing one is not changing the skin. That cost me a round trip on this branch.
[`src/skin_templates/skin-guide.md`](src/skin_templates/skin-guide.md) was
rewritten and documents all 14 variables.

---

## Packaging, which I looked at but deliberately did not touch

The user asked me to document Linux issues rather than fix them. These are the
ones I found. None of them is urgent, and the first is less bad than it looks.

**`packaging/cargo-sources.json` is stale.** 33 crates in `Cargo.lock` are
missing from it, including `lofty`, `metaflac`, `rustls`, `ring` and `rustfft`.
It was last regenerated on 24 August, and every one of those crates entered the
lock file afterwards.

Before you go and regenerate it, check whether it is still load-bearing.
`dev.sparkamp.Sparkamp.yml` does not reference it anywhere. The `sparkamp`
module uses `type: dir, path: .` and relies on the locally vendored `vendor/`
directory being present in the copied tree, which is why `build.yml` runs
`cargo vendor` before invoking flatpak-builder. So the stale file breaks nothing
today. The real question is whether the offline-sources approach is coming back
or whether the file should be deleted along with its section of
`packaging/README.md`. That is a call for the human, not a cleanup to do
silently.

**`packaging/README.md` is out of date.** It documents installing
`org.freedesktop.Platform//23.08`. The manifest is on GNOME 50 with
`org.freedesktop.Sdk.Extension.rust-stable//25.08`, and `build.yml` was updated
to match. Anyone following the README gets the wrong runtime.

**Manifest permissions grew.** `--device=all` (the only grant that exposes
`/dev/srN`), `--filesystem=/run/media:rw` and `--filesystem=/media:rw`,
`--share=network` for gnudb, and `--socket=x11` in place of
`--socket=fallback-x11`. New modules build libburn, libisofs, libisoburn and
cdparanoia. If a Flathub reviewer ever asks about `--device=all`, the comment
above it in the manifest is the answer.

**`gtk-check.yml` gained `libadwaita-1-dev`.** It had been failing at
libadwaita-sys' build script since the toast and empty-state work landed. Build
Flatpak did not catch it, because the GNOME runtime bundles libadwaita.

---

## What is macOS-only and inert for you

Do not spend time wiring these up on Linux. They exist because the macOS App
Sandbox has a specific and badly-named limitation, and they have no Linux
analogue.

- `sparkamp_volume_needs_grant` / `_grant` / `_forget_grant` in
  [`src/ffi/devices.rs`](src/ffi/devices.rs), the `volume_grants` table, and
  `frontends/SparkampMac/Sources/VolumeAccess.swift`. The
  `files.removable-media.read-write` entitlement does not grant reads inside
  mounted volumes. Only a user-selected path plus a security-scoped bookmark
  does.
- `parse_mmc_toc` in [`src/disc/detect.rs`](src/disc/detect.rs) and all of
  [`src/disc/discrecording.rs`](src/disc/discrecording.rs).
- `fs_visible` is computed by an actual `read_dir` on macOS. In the Linux
  detection path it is hardcoded `true` at
  [`src/devices/detect.rs:205`](src/devices/detect.rs:205), which is correct
  there: a mounted block device is readable. Leave it.

---

## Conventions on this branch

No em dashes, anywhere, in prose or code comments. The user has corrected this
twice. If you want to check your own output, decode as UTF-8 first; a byte-level
grep matches every multibyte character and tells you nothing.

Never commit without being asked. Plan checkboxes and past cadence do not
authorize a commit.

Do not run physical drive mechanism commands (`drutil tray eject`, and whatever
the Linux equivalent is) unless asked. I wedged a drive doing that and the user
had to unplug it.

---

## What has not been verified

The baseline, so you know what a regression looks like: on macOS at commit
`1acf808`, `cargo test` is green at 965 passed, 0 failed, 45 ignored. The
ignored ones need real hardware, a disc in a drive or a network. Expect a
different count on Linux, because the cfg-gated tests differ on each side.

What nobody has checked, stated plainly so you do not inherit false confidence:

- The GTK build has not been compiled or run since the `AudioBackend` seam went
  in. Only `cargo test` was run, on macOS.
- The Flatpak build has not been run on this branch since the GNOME 50 bump plus
  the lofty addition.
- The new lofty tag-writing path has been exercised on macOS only.
- Whether a Linux `cdrskin blank=fast` genuinely blanks a disc, or merely
  reports that it did, is unknown. See the erase section above.
- `packaging/README.md` and `packaging/cargo-sources.json` are known stale and
  were left alone on purpose.
