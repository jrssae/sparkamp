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

---

# Linux pass, 3 September 2026

Done on Linux against the handoff above, at `1039ad6`, in the `dev-box`
distrobox after a fresh `cargo vendor`. Numbers before anything changed:
`cargo build --all-targets` passed with 38 warnings, and `cargo test` failed
one test out of 1,192.

## The failing test was a real panic, not a macOS artefact

`duration_probe::tests::a_path_with_a_nul_byte_is_not_measurable` was written
for the AVFoundation prober, which guards against an interior NUL because
`+[NSURL fileURLWithPath:]` answers nil for one and objc2 turns that into a
panic. The GStreamer prober had no such guard: it built `file://{encoded}` and
handed it to `Discoverer::discover_uri`, where glib's C string conversion
panicked with `GStrInteriorNulError(14)`.

Both backends therefore panicked from a function that returns an `Option` and
is called on Rayon workers. The guard moved up into the shared
`discover_duration`, which is where the contract lives; neither backend was
touched. The macOS guard is left in place, because it is right and because
this pass compiles nothing for macOS.

## Zero warnings, and why there were 38

`main` builds with none, so every one of them arrived on this branch. The cause
is structural rather than sloppy: `src/lib.rs` declares `pub mod ffi` and
`src/main.rs` does not, so the binary recompiles the whole tree as a second
crate without it. In a library, a `pub` item reachable from the crate root is
exempt from dead-code analysis; in the binary those same items are private and
unreachable. Every new macOS-facing or FFI-facing item the branch added
therefore reads as dead in the bin target only, which is why the lib target
showed just two of them.

Fixed per item rather than by silencing modules, so real rot still surfaces:

- Items whose callers are macOS-only (`wav_redbook_span`, `track_span`,
  `RED_BOOK_BITS`, `RipFormat::Flac`, `TocEntry`, `toc_from_points`, and the
  whole of `replaygain/rg1.rs` and `replaygain/coefficients.rs`) carry
  `#[cfg_attr(not(target_os = "macos"), allow(dead_code))]`. That is a no-op on
  macOS, so a genuinely unused item there still warns.
- Items whose only caller is `src/ffi` (`read_artwork`, `is_taggable`,
  `EraseGoal::MakeBlank`, `RipFormat::name`, `RipFormat::has_quality`) carry a
  plain `allow` naming the bin/lib split.
- Three items have no production caller at all on any platform and are held up
  only by their own tests: `cdtext::build_v07t`, `sandbox::held`, and
  `XmcdEntry::is_empty`. They are annotated and called out here rather than
  deleted, because removing public API is the human's call.
  `build_v07t` in particular looks like a leftover: the burn path derives a
  `CdTextSheet` once and renders it with `render_v07t`, which is exactly the
  split its own doc comment describes.

## New tests

`tests/macos_license_isolation.rs` enforces the licence boundary the manifest
and CLAUDE.md describe in prose. It reads `cargo metadata --filter-platform`
for both Apple targets, so it sees the resolved graph rather than the manifest
text, and asserts two things: no third-party crate is GPL or AGPL, Sparkamp's
own two crates being the whole exception; and no GStreamer binding resolves at
all. GStreamer needs its own check because a licence scan will not catch it,
the Rust bindings being MIT while the C library they link is not.

Both were watched failing before being kept. Widening the `cfg` gate on the
gstreamer dependencies made the second fail and named all ten crates that
arrived, most of them transitive. A GPL crate added under the macOS target made
the first fail.

`tests/tag_write_containers.rs` covers the lofty routing on Linux. There was
already a thorough eleven-format round-trip at `src/id3_editor.rs`, but it is
`#[ignore]`d behind a `SPARKAMP_FIXTURES` directory the caller builds with
ffmpeg, so nothing about the routing ran in a plain `cargo test`. That ignored
test has now been run here against generated fixtures and passes for aiff,
flac, m4a, mp3, ogg, opus and wav, with wma correctly refused and unmodified.
The two new tests run always, against a tenth of a second of sine tone in
`tests/fixtures/`, and read back through something other than the writer:
`metaflac` for the FLAC comment, and Symphonia for whether the audio still
parses. Forcing `is_mpeg` to return true made both fail with `ID3\x03` where
`fLaC` and `OggS` belong, which is the exact bug the routing was added to fix.

## Packaging

`packaging/cargo-sources.json` is deleted. Nothing referenced it: the manifest
uses `type: dir, path: .` and relies on the vendored tree, which is why
`build.yml` runs `cargo vendor`. `packaging/README.md` is rewritten around what
the manifest actually does and now names GNOME 50 and rust-stable 25.08 instead
of freedesktop 23.08. The requirement comment at the top of `build.yml` said the
generated file must exist, and now says to vendor instead.

One user-facing bug fell out of that: `build.yml` printed
`flatpak run dev.sparkamp.Sparkamp --ui` into the CI job summary, and clap
rejects `--ui`. The GUI is the default invocation and `--tui` is the only mode
flag. Fixed. Several historical plan documents still say `--ui`; they are
records of past work and were left alone. `requirements-osx.md:13` also has it
and is worth a look at some point.

## Still not verified

- **Erase and burn against real hardware. TODO, deliberately not attempted.**
  Whether a Linux `cdrskin blank=fast` genuinely blanks a disc, or merely
  reports that it did, is still unknown. The macOS side grew a verify-then-
  escalate ladder because a quick erase there resets the drive's cached media
  descriptor whether or not it wrote anything, so the drive answers "blank" to
  a question it cannot honestly answer. Linux may have the same problem through
  cdrskin. Testing it needs a rewritable disc and drives the optical mechanism,
  which the handoff above warns against doing unasked, so it waits for a human
  with a disc they do not mind losing.
- The GTK application has been compiled and its tests pass, but nobody has
  launched it and listened to audio since the `AudioBackend` seam went in.
  Playback, seeking, the equalizer and the visualizer all route through the new
  indirection.
- The lofty write path is now exercised on Linux by tests, but not by a human
  against a music library they care about.

---

# Second Linux pass, 3 September 2026

This one is Linux-first. The App Store work above changed shared core and left
GTK and the TUI behind in places, and the point of this pass was to close that
rather than to add anything for macOS. Nothing here compiles for macOS, and the
one change with a cross-platform effect is called out below.

## Parity items brought to Linux

Reviewed every macOS change on this branch for something Linux should also
have, then did them.

- **Skin.** The GTK CSS generator derived muted text at 60% while
  `skin-guide.md` documented 72% and the macOS theme already used 72%. Linux
  rendered one number and shipped a guide claiming another. Now 72%, pinned by
  a test on both built-in skins. The rip-destination caution was also painted
  `dim-label` when the guide names that exact case as a `broken-color` one.
- **About.** The Winamp non-affiliation line, the plain-words no-warranty note,
  and the privacy policy link, all of which macOS had and GTK did not.
- **Per-device rescan.** The disc view had no Rescan at all, and the device
  Scan re-read whatever the cached entry described. Both fixed. macOS reached
  the same place through a synchronous poll; GTK's refresh is asynchronous, so
  the Scan chains through its completion callback instead.
- **Standalone erase.** Erasing was reachable only as a step of a burn, so a
  rewritable disc with content could not simply be blanked. The button sits in
  the burn panel rather than the disc header, because that is where the
  progress row and running flag live and the panel is visible for exactly the
  media states the action applies to.
- **The name.** "ID3 editor" became "tag editor" across GTK and the TUI, as it
  already had on macOS. Tag writing stopped being ID3-only earlier on this
  branch.

## The tag editor, which was the real work

macOS asks the core which fields a container can store, through
`sparkamp_tag_supports_field`. Linux had the same core API and never called it,
which is why `is_taggable` and `supports_frame` looked like dead code in the
first pass. So on Linux a FLAC was offered a URL row it cannot hold, and WMA
and TTA got a full form whose Save could only fail.

Wiring that up exposed something worse underneath. Fields were mapped to an ID3
frame and that frame was then translated into the target format, so ID3 was the
vocabulary every other container was described in. Anything ID3 lacks was
invisible, and a field whose best key differs per format silently got ID3's.
Two fields were being dropped with no error:

- BPM on every Vorbis container, because the writer asked for
  `ItemKey::IntegerBpm`, which lofty documents as ID3v2 and MP4 only. `BPM` is
  a perfectly standard Vorbis field.
- Lyrics on every non-MP3 container carrying an ID3 tag, because lofty
  deliberately maps `USLT` to `UnsyncLyrics` and does not support
  `ItemKey::Lyrics` for ID3v2 at all.

Fields now carry candidate keys, best first, resolved against each target tag
type. **This is the one change here that affects macOS**, since the tag layer
is shared: it fixes the same two silent drops there and changes no behaviour
that was already correct.

To test any of this the repository needed real files, so `tools/make-test-tones.sh`
generates one tone per container and eleven of them are committed, about 28 KB.
Monkey's Audio and Musepack are absent because no encoder for either exists
here. They are real audio rather than headers, so they serve rip and burn tests
too, and the script takes Red Book parameters for a burn that will actually
write.

The test that found both bugs asserts the invariant worth having: a field the
capability gate calls storable must survive a write and a read, on every
container, twice, because tagging a fresh file and editing a tagged one are
different paths.

`cargo test --lib print_field_matrix -- --ignored --nocapture` prints which
native key each field resolves to per container, which is the fastest answer to
"why is this field missing on that format".

## A TUI bug this uncovered

The TUI editor's `focused` was simultaneously the field index, the render row
and the column-split offset, which is why its field list could not vary. Making
rows named rather than positional fixed that, and revealed that
`ID3_FOCUS_COUNT` was a hardcoded 13 while the form rendered 19 rows. Composer,
Original Artist, Copyright, URL, Encoded By and Lyric were drawn and could
never be focused; Tab wrapped before reaching them. Nothing marked them
read-only and GTK has always edited all six. They are editable now.

## Corrected from the first pass

The first Linux pass reported that WAV and AIFF lose fields on re-tagging. That
was measured on files an earlier test had already written and **does not
reproduce** on fresh fixtures, even across two edit passes. `write_lofty_items`
does call `save_to_path` once per tag type inside its loop, which is worth
knowing, but there is no evidence it loses data. Treat the earlier claim as
withdrawn.

## Known gaps, deliberately left

- **BPM on WavPack.** APEv2 conventionally stores a `BPM` item and lofty's APE
  key table has no entry for it, so there is no candidate key to choose. Fixing
  it means writing the raw APE item, which nothing here does yet.
- **Commonly-used tags the editor still does not offer.** Checked against
  MusicBrainz Picard's mapping table: compilation (TCMP / COMPILATION / cpil),
  rating (POPM), disc subtitle (TSST), work and movement, catalogue number,
  barcode, original date (TDOR), album-artist and composer sort, the
  MusicBrainz IDs, and website (WOAR, which is not the WXXX the editor calls
  "URL"). Every one has a lofty key already, so the field set is what is
  missing rather than the plumbing.
- **The macOS FFI still speaks ID3 frame ids.** `sparkamp_tag_supports_field`
  takes a frame id, so the macOS editor remains frame-keyed while GTK and the
  TUI are field-keyed. A field-id entry point is the next step.
- The erase and burn hardware test from the first pass is still outstanding.
