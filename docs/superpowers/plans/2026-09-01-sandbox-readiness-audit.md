# Sandbox readiness audit

> **For agentic workers:** an audit, not a plan. It says what App Sandbox
> breaks and where, so the work can be scheduled. Each section ends with what
> would have to be true, not with steps.

**Scope:** what stops `com.apple.security.app-sandbox` being switched on for the
Mac App Store build. Audio is out of scope: it is solved, see
`2026-08-31-audio-backend-seam-design.md` and the parity result in
`2026-08-31-macos-audio-backend-spike.md`.

Entitlements drafted alongside this: `packaging/macos/entitlements-appstore.plist`.
That file is separate from `packaging/macos/entitlements.plist` on purpose. The
Developer ID build stays un-sandboxed and keeps
`allow-unsigned-executable-memory` for liborc; the App Store build needs neither,
because it plays through AVFoundation with no GStreamer in the bundle.

---

## What is already handled

**Audio.** `AvBackend` (`src/engine/avf.rs`) removes the three blockers
GStreamer carried: `dlopen`'d plugins behind a shell-script launcher the store
rejects, liborc's RWX pages, and ~40 MB of dylibs to sign and licence-audit. EQ
parity measured at 0.13 dB RMS on mid bands against `equalizer-10bands`.

`DefaultBackend` is still `gst::GstBackend`. Flipping it is one line and is
gated on the items below, not on the audio work.

---

## 1. Process spawning. The largest blocker.

App Sandbox blocks `posix_spawn` of anything outside the app bundle.

> **Resolved 2026-09-02. This section's original claim was wrong** — it said
> every macOS-reachable spawn was `/usr/bin/drutil`, and three were not. All of
> them are now gone; what follows is the closed-out list.

| Call site | What it did | Replaced by |
|---|---|---|
| `src/disc/cdtext.rs` | reads CD-TEXT | `DKIOCCDREADTOC` format 5 + `DRCDTextBlock` |
| `src/disc/detect.rs` | the shared `run()` helper: detection and status | `DRCopyDeviceArray`, `DRDeviceCopyInfo`, `DRDeviceCopyStatus` |
| `src/disc/detect.rs` | eject | `DRDeviceEjectMedia` |
| `src/disc/burn.rs` | burning and erasing | `DRBurn*` / `DRErase*` |
| `src/disc/detect.rs` | **`mount`(8)**, for a data disc's mount point | `getfsstat(2)` |
| `src/disc/detect.rs` | **`plutil -convert xml1`**, for an audio CD's `.TOC.plist` | `CFPropertyListCreateWithData` |
| `src/ffi/dedupe.rs` | **`open`(1)**, to reveal a file in Finder | `-[NSWorkspace activateFileViewerSelectingURLs:]` |

The last three were missed by the original audit. The `mount` and `plutil` pair
were hiding behind the same `run()` helper as `drutil` and so read as part of
it; the `open` spawn was in the FFI layer, nowhere near `src/disc`, and had no
`cfg` gate at all — it was the only one reachable on macOS with no Linux
counterpart.

Each replacement also deleted a text parser, which is the same trade the
DiscRecording port made: `getfsstat` hands over `f_mntfromname` and
`f_mntonname` as separate fields, and CoreFoundation decodes the binary plist
that `plutil` existed to convert. What survives is the pure matching logic —
`mount_for_device` and `toc_from_points` — which still has tests on every
platform.

`src/display_backend.rs` spawns five times; every one is
`#[cfg(target_os = "linux")]` or test-only. Not a macOS concern. One
`drutil tray close` remains in `burn.rs`, inside a `#[cfg(test)]` live-test
helper that asks the drive to take a disc back; it does not ship.

**What would have to be true.** `drutil` is a CLI over `DiscRecording.framework`,
so the framework can do all four directly, in-process, with no spawn. That is
the `drutil` → `DiscRecording` item already named in the seam design. It is the
single largest piece of remaining work and it is entirely macOS-side.

**A cheaper alternative worth costing first:** ship the App Store build without
optical disc support. The audit cannot make that call. It is a product decision
about whether a Mac App Store audience burns CDs, and it should be made
deliberately rather than by discovering the port is expensive.

## 2. User-picked folders. Necessary but not sufficient.

`media_library::scan::add_folder` takes `path: &str` and library roots are
persisted as plain path strings in SQLite. In a sandbox a path string grants
nothing after relaunch: the grant lives in a security-scoped bookmark, not in
the path.

So `files.user-selected.read-write` and `files.bookmarks.app-scope` are both
required and neither is enough on its own.

**What would have to be true.** Every persisted library root stores a bookmark
beside its path. On startup each is resolved and
`startAccessingSecurityScopedResource` is called before any scan or watch
touches the tree, and balanced with a stop when the app is done. A root whose
bookmark is stale needs a re-grant through `NSOpenPanel`, which is a UI flow
that does not exist yet.

This is asymmetric work: there is no Linux counterpart, so it belongs behind a
`#[cfg(target_os = "macos")]` module with a documented no-op elsewhere rather
than behind a trait pretending both platforms have the concept.

**Done, 2026-09-02**, in `src/sandbox.rs` and the two halves that use it:

- `folders` gains a `bookmark BLOB` column by the same additive migration the
  table's `recurse` column used. NULL for rows written before it existed and
  for every row written outside a sandbox, both ordinary.
- `add_folder` takes the bookmark **at pick time**. Later is too late: the grant
  belongs to the launch the user picked in, and only a bookmark made while it
  holds survives a restart.
- `restore_folder_access` resolves them at startup and holds each grant for the
  life of the process, in `sandbox::GRANTS`. `Access` releases on drop, and that
  vector is the only owner, which is what makes the start/stop pairing something
  the type system keeps rather than a convention.
- A **stale** bookmark — resolved, but the folder moved — is re-made from where
  it resolved to *and* the stored path is moved with it. Both halves or neither:
  a refreshed bookmark under a stale path would leave every path-keyed lookup
  pointing where the folder no longer is.
- A bookmark that will not resolve is returned to the caller, which reports it.
  Re-granting is an `NSOpenPanel` flow and still does not exist.

Called from `sparkamp_create`, not from `MediaLibrary::open`: background threads
open their own connections, and the grants belong to the process rather than to
a connection.

An existing test caught the one sharp edge — `+[NSURL fileURLWithPath:]` answers
nil for a path with an interior NUL, which objc2 turns into a panic rather than
a `None`. Such a path now makes no bookmark, which is what it is.

## 3. Removable media

macOS presents an audio CD as one AIFF per track on a mounted volume, and
`src/disc/toc.rs` already reads it that way. Portable players mount under
`/Volumes` too.

`files.removable-media.read-write` covers it. Note this does **not** grant
silent enumeration of arbitrary volumes; the user still has to be the one who
points at a device.

## 4. Config and data paths

17 call sites use `dirs::config_dir`, `dirs::data_dir` or `dirs::cache_dir`.
Under sandbox those resolve inside the container automatically, so **no code
change is needed for new installs**.

The problem is existing ones. A user upgrading from the DMG build has a
populated `~/Library/Application Support/sparkamp` that the sandboxed app
cannot see, and would silently appear to have lost their library, playlists and
skins.

**What would have to be true.** A one-shot migration on first sandboxed launch,
idempotent (per `make-operations-idempotent`), that copies rather than moves, so
a user who goes back to the DMG build still has their data. This needs a
decision about whether the two builds are expected to coexist on one machine.

## 5. What the entitlements file does not request, deliberately

- **`allow-unsigned-executable-memory`.** Only GStreamer's liborc needed it. The
  App Store build has no GStreamer. Requesting it invites review questions for
  nothing.
- **`files.all` / `files.downloads`.** Nothing in the tree reads a fixed
  location outside the container. Library access is user-selected.
- **`device.usb`.** Portable-player sync goes through mounted volumes today. If
  raw MTP is ever wanted, that is a separate conversation with App Review, and
  ImageCaptureCore is the sanctioned path.
- **`network.server`.** The app is a client (gnudb over http via `minreq`).

## Decisions (Josef, 2026-09-01)

**1. Full feature parity is the guiding principle.** The App Store build ships
as close to 100% of the DMG build's features as possible, and any gap needs
serious review rather than a shrug.

Consequence: **the `drutil` → `DiscRecording` port is required work, not an
option.** The cheap escape in section 1 is closed. All four call sites move to
the framework. This is now the critical path for the sandbox, and it is the
largest single remaining piece of the App Store MVP.

It also raises the bar on anything else that would quietly degrade on macOS.
The ReplayGain capability gap in `AvBackend` (`clip_protection` and
`album_mode` are inert) is a parity gap by this standard, not a footnote.

**2. The two builds do not coexist on one machine.** A user has the DMG build
or the App Store build, not both.

Consequence: section 4's migration does not need to leave the old location
intact for a downgrade path. It should still **copy, verify, then remove**
rather than rename in one step, because a migration interrupted halfway must
not lose a library. That is crash-safety, not coexistence.

**3. Bundle identifier: `dev.sparkamp.Sparkamp`.**

The tree already answers this. `dev.sparkamp.Sparkamp` appears 18 times
directly, and the Flatpak manifest, `metainfo.xml`, `.desktop` entry and icon
are all named after it. `CLAUDE.md:37` states the rule outright, and
`CLAUDE.md:35` fixes the casing as "Capital S, lowercase a".

`com.sparkamp.sparkampmac` exists in exactly two places, both in
`frontends/SparkampMac/SparkampMac.xcodeproj/project.pbxproj`, and breaks the
rule twice over: wrong prefix and wrong casing.
`docs/mac-pass-checklist.md:2189` refers to `dev.sparkamp.SparkampMac` from an
earlier session, so the `com.` value reads as drift rather than a decision.

Change `PRODUCT_BUNDLE_IDENTIFIER` at `project.pbxproj:528` and `:578`. Cheap
now; expensive once an App ID is registered against the old value.

## 6. GStreamer is still linked, and still needed (found 2026-09-02)

Not in the original audit, and it is a blocker.

The audio backend switch removed GStreamer from *playback* on macOS, but the
dependency is unconditional in `Cargo.toml` and two things still reach it:

- **`sparkamp_create` calls `gstreamer::init()` and returns null if it fails.**
  An App Store build that ships no GStreamer plugins therefore has no way to
  start — the app is dead at launch, not degraded.
- **Burning transcodes through GStreamer.** `prepare_wav` turns any source into
  a Red Book WAV, and that is a GStreamer pipeline. The DiscRecording port
  replaced the `drutil` spawn, not the staging.

**Burn staging: done, 2026-09-02.** `src/disc/transcode.rs` is a seam in the
same shape as `engine::backend` — a `Transcoder` trait whose vocabulary is the
job (a source, a destination, progress), two adapters, and a `cfg`-selected
default. The GStreamer pipeline string moved out of `burn.rs` and into the
GStreamer adapter, where it belongs; before this the core burn module knew what
a `decodebin` was.

Measured across all eight formats macOS decodes: every one comes out
44.1 kHz / 2 ch / 16-bit, and the dominant frequency of the result is the
440 Hz tone that went in. Shape alone would have passed on silence.

**Ripping: FLAC on macOS (Josef, 2026-09-02).** CoreAudio decodes MP3 without
being able to write it, so a format change was the choice made rather than
bundling an encoder. FLAC is lossless, so the macOS default is a different
format and not a lesser one.

`Encoder` is the second half of the same seam: `RipFormat`, a `default_format`
and a `can_write` each platform answers for itself, and a caller that asks
rather than assumes. `dest_path` takes the format instead of hardcoding
`.mp3`, and the tag container follows it — a FLAC gets Vorbis comments, since
an ID3 tag on a FLAC is, to every FLAC reader, no tags at all.

**A third metadata source, found on real hardware.** The test disc is an
8-track CD-R that macOS mounted as *Covers From Another Mother* with every
track named — and whose drive answers the CD-TEXT ioctl with `EIO`. It has no
CD-TEXT at all. The names came from macOS's own lookup, and they were sitting
in the mounted filenames while the detector reported "Track 1".."Track 8" and
ripped files called `01 - Track 1.flac`.

`track_entries` now takes each title from its mounted filename. It costs
nothing and needs no network — the lookup already happened — and it is only a
starting point, since a gnudb or CD-TEXT match overwrites it and the rip window
overwrites that.

The placeholder problem is that an unresolved disc names every track "Audio
Track", **localized**, so the words cannot be matched on. What can be matched
on is that they are all identical, which no real track list is. A single-track
disc is trusted anyway: the two mistakes are not symmetric, and losing a disc's
only title is the worse one.

**The precedence chain, proven on hardware.** A 15-track disc carrying 940
bytes of real CD-TEXT — *Bespoke Bounce* by the Waller Creek Vipers — and no
macOS lookup, so CD-TEXT was the only source. `live_rip_window_overrides_the_disc`
reads it, prepopulates as it would with no gnudb entry, edits the album and one
track title the way a user would, and rips:

| | title | album |
|---|---|---|
| track 1 (edited) | `A Title The Disc Never Had` | `Edited In The Rip Window` |
| track 2 (untouched) | `Byas A Drink`, from CD-TEXT | `Edited In The Rip Window` |

The edit reaches the filename, the directory and the Vorbis comments; the
untouched track keeps what the disc said. That asymmetry is the whole assertion
— everything else in the rip path can be right while a value the user typed is
quietly replaced by one from the disc, and that failure looks exactly like
success until you read the tags back.

**Metadata precedence, settled at the same time.** The rip window is
prepopulated from gnudb with the disc's own CD-TEXT filling anything gnudb did
not carry, and with no gnudb entry it is CD-TEXT alone. Whatever the user then
enters or overrides in the window is what gets ripped: nothing downstream reads
the disc again to second-guess a field they cleared on purpose. The rule is
`XmcdEntry::merged_with`, in core, exposed to both frontends through
`sparkamp_disc_merge_metadata` — two copies of it would drift, and the symptom
would be a disc that tags differently depending on which UI ripped it.

**Still outstanding for "does not ship GStreamer":** ripping (above),
`replaygain.rs`'s analysis pipeline, `duration_probe.rs`'s Discoverer fallback,
and `gstreamer::init()` as a launch precondition in `sparkamp_create`.

## Not audited

Signing and provisioning (`Apple Development` today, App Store needs
`Apple Distribution` plus a matching profile), and the `export-options.plist`
method change. Those are packaging, not code, and are blocked on the bundle-ID
decision rather than on anything here.

## Reopened: the licence (2026-09-02)

The working decision recorded elsewhere in this effort — keep AGPL-3.0 and add
an App Store exception — is **no longer settled**. Josef's position is that an
exception will not suffice and Apple will decline it, leaving relicensing the
project outright as the alternative.

That is a project-level decision, not a packaging one, and it is a discussion
rather than a task. See `2026-09-02-license-for-the-app-store.md` for what the
discussion needs. No engineering work in this effort is blocked on it.
