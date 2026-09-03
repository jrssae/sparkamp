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

### Reversed, 2026-09-02: macOS keeps `com.sparkamp.sparkampmac`

The reasoning above was sound and the conclusion was wrong, because it was
reached from the tree alone without asking what already existed outside it.

Three facts settle it the other way:

- **Every released build has used `com.sparkamp.sparkampmac`,** including
  v1.3.3. The change in `0fb29ad` never shipped. The audit read the `com.`
  value as uncorrected drift; it was the shipped identifier.
- **An App ID and an App Store Connect record already exist against it,** and
  an App Store record that has never had an approved version **can never be
  deleted** — that is Apple's policy, not a UI problem. The identifier is
  permanent whichever way this goes.
- **A bundle ID is invisible to users.** It appears nowhere in the store
  listing, in search, or on the product page. The store name is a separate
  field, and a record whose bundle ID is `com.sparkamp.sparkampmac` lists
  perfectly well as "Sparkamp".

So aligning the two identifiers would have cost existing users their saved UI
state — 26 `UserDefaults` keys, keyed by bundle ID — to buy nothing anybody can
see. The library, playlists, config and skins were never at risk either way:
the Rust core keeps them in `~/Library/Application Support/sparkamp/`, which is
not keyed by bundle ID.

**The two identifiers now differ deliberately**, and `CLAUDE.md` states both
rather than one rule the code quietly breaks:

| | |
|---|---|
| Linux / Flatpak app id | `dev.sparkamp.Sparkamp` |
| macOS bundle id | `com.sparkamp.sparkampmac` |

The casing objection stands and is simply outweighed. It is worth recording
that this was decided rather than overlooked, because the next person to read
`packaging/dev.sparkamp.Sparkamp.desktop` beside the Xcode project will assume
otherwise.

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

### Done: macOS does not link GStreamer at all (2026-09-02)

Measured on the artifact that ships, not inferred from the source:

```
nm target/debug/libsparkamp_macos.a | grep -c gst_   →  0
cargo tree --manifest-path frontends/macos/Cargo.toml | grep -c gstreamer  →  0
```

The dependency is now `cfg(not(target_os = "macos"))`, so it is not merely
unused there — it is not in the graph.

What moved, and where each went:

| Was GStreamer | Now |
|---|---|
| playback | `engine::avf`, via `DefaultBackend` |
| burn staging | `disc::transcode::avf` |
| ripping | `disc::transcode::avf`, to FLAC |
| duration fallback | `AVAudioFile`, via `duration_probe::platform` |
| `gstreamer::init()` as a launch precondition | gated; macOS needs nothing to be up |

That last one was the blocker. `sparkamp_create` returned null on a failed
init, so a build with no plugins was not a degraded app — it was a bounce in
the Dock.

**ReplayGain analysis: implemented, 2026-09-02.** See below; this paragraph
described the gap before it was closed.

**One feature did not survive the GStreamer removal: ReplayGain analysis.** `rganalysis` is a
GStreamer element, and there is no CoreAudio equivalent — the alternative is
implementing the ReplayGain algorithm, which is its own piece of work and was
deliberately not smuggled into this change. `rg_analysis_available()` answers
`false`, and the UI already gates the action on it, so the feature is absent
rather than broken. **Playback is unaffected:** applying a gain the library
already holds is arithmetic and needs no analyser.

Everything above the seam in `replaygain.rs` — the number formats, the tag
parsing, the album batching, the manual edits — is shared and untouched. Only
the measuring moved behind a platform module.

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

**Reviewed 2026-09-02, and the fear does not survive the facts.** Josef is the
sole copyright holder — all 893 commits — and removing GStreamer removed the
last copyleft dependency from the macOS build. A rights holder is not bound by
the licence he grants others, so publishing the source under AGPL-3.0 and
separately shipping his own App Store build needs no exception and no
relicensing. See `2026-09-02-license-for-the-app-store.md`. Nothing in this
effort is blocked on it.

## ReplayGain measured without GStreamer (2026-09-02)

`rganalysis` is a GStreamer element, so the App Store build had no way to
measure new gains. `src/replaygain/rg1.rs` implements ReplayGain 1.0 directly.

**It agrees with `rganalysis` exactly.** On lossless audio, where both read
identical PCM:

```
rganalysis: +6.5300 dB (ref 89), peak 0.085754
this:       +6.5300 dB,          peak 0.085754
delta:      -0.0000 dB,          peak +0.000000
```

Over 39 real MP3s from a library, which measures algorithm *and* decoder
together:

| | median | p90 | max |
|---|---|---|---|
| gain | 0.0000 dB | 0.0300 | 0.0900 |
| peak | 0.000000 | 0.000000 | 0.105822 |

Half match exactly. The spread is the decoders disagreeing, not the algorithm:
CoreAudio and GStreamer do not decode MP3 bit-identically (measured separately
at mean 4.7 LSB, max 2809 of 32767). **0.01 dB would have been too tight a bar
for lossy input** and exactly right for lossless, which is why the two are
tested separately.

### Where it lives, and why

The measuring is in `rg1`, **compiled and unit-tested on every platform** even
though only macOS routes to it. An implementation gated to one platform is one
the other's CI never builds, and the first anyone would hear of a break is a
user with wrong gains. Only the decode is macOS's.

Above that, `replaygain::analyze_batch` is unchanged and knows nothing: it
calls `analysis::analyze_batch`, and a `cfg` picks `rganalysis` or this. Linux
and the TUI are untouched.

### Three things the reference does that are easy to get wrong

- **It measures integer PCM, not normalised floats.** Its 0–120 dB histogram is
  anchored to 16-bit scale, so normalised samples land every window in bin 0
  and every track reports the same 64.82 dB. Caught immediately: every gain
  was identical.
- **The histogram bin is truncated, not rounded.** Rounding shifts every
  measurement half a bin.
- **Album gain accumulates every track's windows into one histogram** and takes
  the percentile once. Averaging the track gains gives a different, wrong, and
  entirely plausible-looking answer. Pinned by a test that fails under
  averaging.

`PINK_REF` of 64.82 is already anchored to `rganalysis`'s 89 dB reference. A
draft that "corrected" for the specification's 83 dB was wrong by exactly
6.0000 dB, which is how it was caught — a fudge factor that is exactly a round
number is a fudge factor that is wrong.

## The top open risk: CD-TEXT reads a raw device node (2026-09-02)

Every other sandbox question in this audit is now settled or implemented. This
one is not, and it cannot be settled from here.

`read_cdtext_packs` opens the media's raw BSD node — `/dev/rdiskN` — and
issues `DKIOCCDREADTOC`. That is the documented way to get CD-TEXT, and it is
what `DRCDTextBlockCreateArrayFromPackList`'s own documentation points at.

**Whether the App Sandbox permits it is unknown.**
`com.apple.security.files.removable-media.read-write` is documented for *files
on removable volumes*, and a device node is not a file on a volume. It may be
covered; it may not.

There is no legal fallback if it is denied:

- `DRDeviceReadCDText` exists in the framework but is **undeclared**, and App
  Review treats undeclared symbols as private API.
- The mounted volume's `.TOC.plist` carries the table of contents — sessions,
  track points, start blocks — not CD-TEXT. Reading it would not help.
- CD-TEXT lives in the disc's lead-in. There is no filesystem path to it.

**What settles it:** a signed sandboxed build, a disc with CD-TEXT (the
15-track *Bespoke Bounce* disc used for the rip tests carries 940 bytes of it),
and `live_cdtext_absence_is_quiet` — which reports `Absent` versus a read
failure and so distinguishes "denied" from "no CD-TEXT on this disc".

An attempt to approximate the answer with `sandbox-exec` was inconclusive and
would not have been authoritative anyway: `sandbox-exec` profiles are not App
Sandbox entitlements.

**Do not paper over it** with a `temporary-exception` entitlement before
measuring whether one is needed. If it turns out to be denied, the honest
options are to ship without CD-TEXT reading on the App Store build — writing it
during a burn is unaffected, since that goes through DiscRecording — or to
request an exception with a measured justification.

## Live regression against an audio CD, after the GStreamer removal (2026-09-02)

None of the disc paths had been exercised since GStreamer left the macOS build.
Run against the 15-track *Bespoke Bounce* disc, which carries 940 bytes of
CD-TEXT and which macOS has **not** resolved online — so it mounts as
`Audio CD` with every track named `N Audio Track.aiff`.

| | |
|---|---|
| `live_list_drives` | 15 tracks, TOC and mount resolved |
| `live_cached_poll` | 1.71 ms warm |
| `live_second_poll_is_cached` | pass |
| `live_read_cdtext_ffi` | pass |
| `live_dump_cdtext_packs` | 940 bytes |

**The placeholder rule met the case it was written for.** Every file on this
disc derives the same title, "Audio Track", so the titles are rejected and the
tracks read `Track 1`…`Track 15`. Until now only the positive case — a disc
macOS *had* resolved — had been verified; this is the negative one.

### ReplayGain on real CD audio

CD tracks are lossless 44.1 kHz AIFF, so this compares the algorithm rather
than the decoders. Against `rganalysis` over four tracks:

| | median | p90 | max |
|---|---|---|---|
| gain | 0.0000 dB | 0.0100 | 0.0100 |
| peak | 0.000000 | 0.000000 | 0.000000 |

Peak is exact. The 0.01 dB on gain is **one histogram bin** — the quantisation
floor of the format, and the closest two independent implementations can get.

### Album gain, on material where a wrong answer would look right

Three tracks: −6.31, −7.53, −8.48 dB. Album **−7.65**; the mean of the track
gains would be **−7.44**.

0.21 dB apart. The synthetic test uses a loud track and a quiet one and the two
answers differ by ten dB, which is easy to catch. Real albums are mastered
close together, and that is where averaging the track gains produces a
plausible wrong number — which is why this is worth measuring on real material
as well.

### What this did not settle

The raw-device question above. That needs a signed sandboxed build; running
unsandboxed only re-confirms what already works.

## DVD, on real media (2026-09-02)

A DVD+RW, which turned out to be the more interesting kind. Nothing in the DVD
path had ever been exercised.

Detection is right: `kind: DvdRw`, rewritable, 4.70 GB capacity. Arriving with
an ISO on it, the disc mounted at `/Volumes/ISOIMAGE` as the whole device with
no slice suffix, which is the case `mount_for_device` handles and had not been
tested against real media.

| | |
|---|---|
| erase | 27.9 s |
| data burn, 3 files plus playlist | verified on the mount |
| erase-first rewrite, 2 files | 153.7 s, replaced the 3 |

### DiscRecording blanks a DVD+RW, and cdrskin does not

The erase test carried a comment from 2026-07-17 saying DVD+RW is overwrite
media with no blank state, because `cdrskin blank=fast` is a compatibility
no-op on it and the old content stays readable. True on Linux.

DiscRecording's erase genuinely blanks it. Measured: 4.70 GB used and 0 free
before, 0 used and 4.70 GB free after, with `Writability: appendable, blank,
erasable, overwritable`.

So the assertion is now by platform rather than by media kind. macOS asserts
the disc probes blank whatever the kind; everywhere else keeps the weaker
invariant that it can still be burned again.

### The CD "no ISO 9660" finding was a reading problem

The DVD burn produced a Primary Volume Descriptor at LBA 16 with the staged
folder's name, `SPARKAMP_HWDATA_37492`. The same code wrote both discs, so the
burn writes ISO 9660 and the earlier CD result was not what it looked like.

The reason is visible in the numbers already recorded: a burned CD carries an
Apple partition scheme, and its whole-disc node reported 1.3 MB where the
session used 1.85 MB. LBA 16 of that node is not LBA 16 of the ISO image.

The data disc is therefore very unlikely to be Mac-only, which is what that
open question was really asking. It stays a report rather than an assertion
until someone finds where a CD's ISO image starts, because asserting it would
fail on CD for a reason that has nothing to do with the burn.
