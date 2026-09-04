# Porting `drutil` to DiscRecording

> **For agentic workers:** the API survey and the shape of the work. Read the
> sandbox audit first (`2026-09-01-sandbox-readiness-audit.md`); this is its
> section 1, expanded because the decision made it required.

**Why it is required.** Full feature parity is the guiding principle for the App
Store build (Josef, 2026-09-01), which closes the cheap escape of shipping
without optical disc support. App Sandbox blocks spawning `/usr/bin/drutil`,
so every one of the four call sites has to reach the framework directly.

| Call site | What it does today |
|---|---|
| `src/disc/cdtext.rs:263` | reads CD-TEXT |
| `src/disc/detect.rs:827` | the shared `run()` helper: detection and status |
| `src/disc/detect.rs:1003` | eject |
| `src/disc/burn.rs:385` | burns an audio CD |

---

## The API to bind, and why this is cheaper than feared

`objc2-disc-recording` **does not exist**. Confirmed against the crates.io API,
not inferred: `{"errors":[{"detail":"crate 'objc2-disc-recording' does not
exist"}]}`. So unlike AVFoundation there is no ready-made binding crate.

That matters less than it sounds, because DiscRecording ships two APIs and the
one we want is **C over CoreFoundation**, not Objective-C. `DRCore*` needs plain
`extern "C"` declarations plus `objc2-core-foundation` (0.3.2, exists) for the
CF types. No Objective-C runtime, no message sends, no hand-written class
declarations.

Everything the four call sites need is present:

| Need | Function |
|---|---|
| enumerate drives (`drutil list`) | `DRCopyDeviceArray` |
| device identity | `DRDeviceCopyInfo` |
| media state (`drutil status`) | `DRDeviceCopyStatus` |
| eject | `DRDeviceEjectMedia` |
| tray | `DRDeviceOpenTray`, `DRDeviceCloseTray` |
| exclusive access | `DRDeviceAcquireExclusiveAccess`, `...Release...` |
| CD-TEXT | `DRCDTextBlockCreateArrayFromPackList`, `DRCDTextBlockGetValue`, `DRCDTextBlockGetTrackDictionaries` |
| burn | `DRBurnCreate`, `DRBurnWriteLayout`, `DRBurnCopyStatus`, `DRBurnAbort` |
| erase | `DREraseCreate`, `DREraseCopyStatus` |

## This is an upgrade, not a translation

`DRDeviceCopyStatus` returns a dictionary whose keys map one-to-one onto the
fields the code currently recovers by parsing `drutil status` text:

`kDRDeviceMediaIsBlankKey`, `kDRDeviceMediaIsErasableKey`,
`kDRDeviceMediaIsAppendableKey`, `kDRDeviceMediaIsOverwritableKey`,
`kDRDeviceMediaBlocksFreeKey`, `kDRDeviceMediaBlocksUsedKey`,
`kDRDeviceMediaBlocksOverwritableKey`, `kDRDeviceIsTrayOpenKey`,
`kDRDeviceIsBusyKey`, `kDRDeviceMediaBSDNameKey`, `kDRDeviceMediaClassKey`.

Structured values replace scraped text, so the port should **delete** parsing
code rather than port it. Anything that survives as a string parse is a smell.

**A second improvement available but out of scope for parity.**
`kDRDeviceStatusChangedNotification` with `DRNotificationCenterAddObserver`
would replace the devfs-fingerprint poll in `detect.rs` with a push. Tempting,
and a real simplification, but it changes the concurrency model. Do the port
first, land it green, then consider it separately.

## Measured baseline, on real hardware

Slimtype DVD A DS8A5SH over USB, audio CD present, 15 tracks, one session,
`/dev/disk12`, mounted at `/Volumes/Audio CD`.

| Test | Result |
|---|---|
| `live_list_drives` | passes |
| `live_data_disc_browse` | passes |
| `live_second_poll_is_cached` | passes |
| `live_drutil_cdtext_read` | passes |
| `live_read_cdtext_ffi` | passes |
| `live_cached_poll` | **was failing**, fixed separately |

A full probe on this drive costs **604 ms**; a warm cached poll costs **854 µs**,
about 700x faster. Those numbers are the parity bar: the ported version must not
be slower, and being a library call rather than a process spawn it should be
faster.

`live_cached_poll` was failing because it timed the wrong call. Only
`list_drives_cached` records the devfs fingerprint, so the first cached call
after a raw `list_drives` always probes. It passed on a fast internal drive and
failed on this USB one at 616 ms. Fixed by warming first, then timing; the fix
is mutation-verified.

## Verification, and its honest limit

Detection, status, CD-TEXT and eject are all verifiable right now against the
attached drive and disc. The live tests above are the reference.

**Burning is not verifiable without consuming media.** Each attempt destroys a
disc, and failure modes (buffer underrun, a drive that lies about supported
speeds, a session that will not close) do not reproduce on demand. Blank CD-R,
CD-RW and DVD-RW are available for a human-run pass, and `live_hw_burn_audio`,
`live_hw_burn_data`, `live_hw_erase` and `live_hw_rewrite_data` already exist as
`#[ignore]`d tests for exactly this.

Sequence the work so burning is last and separable. If the first three land
verified and burning needs another session with media in hand, that is a good
outcome, not a partial one.

## Order

1. The FFI layer: `extern "C"` declarations and CF helpers, no behaviour change.
2. Detection and status. Verify against `live_list_drives`,
   `live_cached_poll`, `live_second_poll_is_cached`, and the 604 ms / 854 µs
   figures.
3. CD-TEXT. Verify against `live_drutil_cdtext_read` and `live_read_cdtext_ffi`
   with the audio CD in the drive.
4. Eject. Verify by hand: it is one call and its effect is visible.
5. Burning and erasing. Needs blank media and a human. Last.

Steps 2 through 4 delete their `drutil` spawn as they land. Step 5 deletes the
final one, and only then is `src/disc` sandbox-clean.

---

## Burn API survey, and blank-media findings (2026-09-01)

Done with a real blank CD-R in the attached drive.

### Burning is feasible in pure C

The remaining `drutil` spawn is `burn.rs`. The C API covers it, including the
part that looked most likely to force Objective-C:

| Need | Function |
|---|---|
| build a track | `DRTrackCreate` with a `DRTrackCallbackProc` |
| pre-flight | `DRTrackEstimateLength`, `DRTrackSpeedTest` |
| burn | `DRBurnCreate`, `DRBurnWriteLayout`, `DRBurnCopyStatus`, `DRBurnAbort` |
| erase | `DREraseCreate`, `DREraseCopyStatus` |

`DRTrackCallbackProc` is `OSStatus (*)(...)`, a plain C function pointer, so
Rust supplies it as an `extern "C" fn`. No blocks, no message sends, same as the
detection port.

### Detection against blank media: correct, and it found a real bug

The ported detector reads a blank CD-R correctly and matches `drutil` exactly:
`is_blank: true`, `kind: CdR`, `free_bytes: 736966656` (736.97 MB), label
"Blank CD-R".

One field is wrong, and it is **not** a regression. `rewritable` comes back
`true` for write-once media, because the derivation includes `is_overwritable`,
and a drive reports a blank CD-R as overwritable meaning its blocks are
available, not that the disc can be erased. `drutil status` says the same thing
(`Writability: appendable, blank, overwritable`). The pre-port code at 5b4ea0b
had identical logic, so the port reproduced it faithfully.

`erase_decision` checks `is_blank` before `rewritable`, so a blank CD-R returns
`None` and never reaches the bad branch. The failure case is a **partially
written, appendable CD-R**: not blank, still reports appendable space, so it is
offered an erase the media cannot perform. Filed separately; the media matrix
tests in `burn.rs` do not cover that case.

### Media budget

Burning is one-shot per CD-R. Spend a disc on the **new** code, not on
re-proving `drutil`, which already works. Everything up to the write is
verifiable without consuming media: layout construction, `DRTrackEstimateLength`,
`DRTrackSpeedTest`, and a `DRBurnCopyStatus` read before starting.

CD-RW and DVD-RW are reusable, so erase and rewrite paths cost media once rather
than per attempt. Sequence those after the CD-R write if discs are scarce.

---

## The first burn wrote nothing, and why (2026-09-01)

A real CD-R burn returned `noErr` after **210.5 ms** and reported success. The
disc was still blank (`drutil status`: `Space Used: 00:00:00`).

### What the status dump said

```
DRStatusStateKey         = DRStatusStatePreparing
DRStatusTotalTracksKey   = 0
DRStatusPercentCompleteKey = -1
DRBurnWriteLayout return = 0 (noErr)
```

Ruled out first, each against the header rather than by guessing: the layout
shape (a flat `CFArray` of `DRTrack`s is exactly the documented single-session
multitrack form), the properties not applying (`DRBurnGetProperties` reads them
back off the real burn object), and `SupportLevel: Unsupported` (the header says
the engine tries anyway; only `SupportLevelNone` means unusable).

### The cause

**`kDRSynchronousBehaviorKey` is ignored.** It is set, it round-trips through
`DRBurnGetProperties` as `true`, and the engine burns asynchronously regardless.
`DRBurnWriteLayout` returns as soon as the burn *could begin* — which is what
its own doc comment says it reports, and not what the synchronous-behaviour doc
promises.

`run_operation` treated the start call returning as the operation finishing. So
it polled once, saw `Preparing`, found no error attached, called it a success,
and dropped the last reference to the burn object — which stopped the burn
before it wrote a byte.

### How it was proved without spending a disc

`kDRBurnTestingKey` runs the entire write path with the laser at low power. The
drive advertises it (`drutil info` → `CD-Write: ... Test ...`). With it set, the
same code was watched past the join:

```
[diag] start returned 0x0 after join
[diag] t+37s  DRStatusStateSessionClose   TotalTracks=2  10.1x  1787 kB/s
[diag] t+38s  DRStatusStateFinishing
[diag] t+39s  DRStatusStateDone
```

39 seconds of real work, two tracks, after the call that was being treated as
the finish line had already returned. The disc came out unmodified.

It also showed `kDRBurnCompletionActionEject` **does** apply — the drive ejected
on completion — which is what separates "properties are ignored" from "this one
property is ignored".

### The fix

The status state machine decides, not the start call. `run_operation` polls
until `kDRStatusStateDone` or `kDRStatusStateFailed`, and:

- a non-`noErr` start return still ends it immediately, because then there is no
  state machine to wait for;
- a state that never leaves `kDRStatusStateNone` for 30 s after a successful
  start ends it too — the allowance is discarded the moment the state advances,
  so a full-disc write is never cut off;
- ending anywhere that is not `Done` is now an **error**, not a success. That is
  the specific bug: silence read as completion.

`kDRSynchronousBehaviorKey` stays set. It costs nothing and the poll loop is
correct either way — a drive that did honour it would just return its start code
at the end instead of the beginning.

Progress reporting changed with it: `kDRStatusPercentCompleteKey` is documented
as 0 to 1 and the engine reports `-1` for phases it cannot measure. That is
unknown, not zero; clamping it dropped the progress bar back to empty while the
disc was being closed.

### `SPARKAMP_BURN_REHEARSE`

The laser-off path is kept as a named environment gate, and it announces itself
on stderr when set. It is the only way to exercise a burn end to end against
write-once media without spending it, and the noise is deliberate: a burn that
quietly wrote nothing is the failure this whole section is about.

### The burn, verified end to end (2026-09-01)

One CD-R, burned through the fixed loop.

| | |
|---|---|
| write | 38.9 s, `DRStatusStateDone`, `burn_audio` returned `Ok` |
| progress | climbed 0.02 → 0.99, `None` only in the closing phases |
| media after | 1 session, 2 tracks, 1656 blocks used, 0 free, closed |
| TOC | `Audio CD (2 tracks)`, track 1 at frame 150, track 2 at 1053 |
| mount | two AIFF tracks, `2 ch, 44100 Hz, Int16` |
| audio | track 1 peaks at **441.4 Hz** — `tone_440.mp3`, through the producer callback and back off the disc |

The frequency check is the one that closes the loop: it proves the `extern "C"`
producer served the right PCM at the right addresses, which no amount of status
polling can tell you.

Watch the endianness if you repeat it. macOS mounts an audio CD as **AIFC with
`sowt`** — little-endian PCM in a big-endian container. Reading it big-endian
gives a plausible-looking spectrum with a peak in entirely the wrong place;
`afconvert -f WAVE -d LEI16` first, or read the COMM chunk's compression ID.

### CD-TEXT, written and read (2026-09-02)

macOS now writes CD-TEXT. It never has: the pre-port code handed `drutil` a
folder and dropped the v07t sheet, and its own comment said so. Verified on a
CD-RW, erased and rewritten repeatedly.

```
album:  "Sparkamp CDTEXT Live"
artist: "Sparkamp Test"
tracks: [(1, "tone_440"), (2, "tone_660")]
```

Written by `DRCDTextBlockCreate` + `kDRCDTextKey`, read back by the app's own
reader. `live_hw_burn_audio` now passes end to end — erase, burn, TOC, CD-TEXT
— for the first time.

**One sheet, two serializations.** `CdTextSheet` in `cdtext.rs` is the source of
truth: sanitized, split into performer and title, in track order. macOS builds a
`DRCDTextBlockRef` from it; Linux renders it to the v07t file `cdrskin` reads,
next to the staged WAVs it describes. Deriving it once is what keeps the two
platforms from disagreeing about what a track's artist is.

CD-TEXT indexes the **disc** at 0 and track N at N. That offset lives in exactly
one place, `cdtext_block`, which is why `CdTextSheet` carries no track numbers.

**`DRCDTextBlockCreate` works, unlike its read-side sibling.** A non-NULL return
proves nothing here — `DRCDTextBlockCreateArrayFromPackList` hands back pointers
that segfault on first use. So `cdtext_round_trip` sets values and reads them
back, and a unit test pins it. No media, no burn.

### The real bug was in the reader, and it had been there all along

The first CD-TEXT burn wrote a correct two-track disc and read back `Absent`.
The obvious conclusion — the burn is not writing CD-TEXT — was wrong.

Dumping the raw PACKs off the disc settled it: **204 bytes**, with
`Sparkamp CDTEXT Live`, `tone_440`, `tone_660` and `Sparkamp Test` plainly
visible. The burn had worked the whole time.

`DRCDTextBlockCreateArrayFromPackList`'s documentation is explicit: *"The CFData
should be sized to fit the exact number of PACKs. Each PACK occupies 18 bytes,
and the 4-byte header from a READ TOC command may optionally be included."* The
reader passed the raw ioctl buffer straight through. The drive reported 204
bytes while the header declared 200 — so the real answer was a 4-byte header
plus 11 PACKs, 202 bytes, with two bytes of slop past the end. Those two bytes
were the difference between reading the CD-TEXT and reporting `Absent`.

`trim_to_whole_packs` cuts the answer to `2 + declared length`, then down to a
whole number of 18-byte PACKs. Four mutants, all killed; the declared-length
bound needed its own case, because on the 204-byte disc the PACK rounding
happens to absorb the slop on its own.

This is a **read** fix. Any disc whose drive over-reports has been unreadable
since the reader was written, burned by anything.

### A fix that was not one

Requesting `kDRBurnStrategyCDSAO` with `kDRBurnStrategyIsRequiredKey` was added
first, on the header's statement that the track-at-once strategy "cannot write
CD-Text". It looked like the fix because it landed in the same change as the one
that mattered.

Isolated afterwards on the rewritable disc: a burn with the block attached and
**no** strategy requested wrote 11 PACKs. The engine already picks a strategy
that can carry the data — "a burn strategy will never be used if it cannot write
the required data" — so both the request and the matching SAO check in
`can_write_cdtext` were removed. `kDRBurnStrategyIsRequiredKey` would have
turned a drive the engine could satisfy another way into a failed burn.

`can_write_cdtext` now asks the drive one question, `kDRDeviceCanWriteCDTextKey`,
which is what the header says to check. A drive that answers no gets a burn
without CD-TEXT rather than `kDRDeviceCantWriteCDTextErr` and no disc at all.

### The erase-first burn was broken, and only the framework port showed it

`run_job` with `erase_first = true` — the "erase and burn" button, the path a
user takes after the erase confirmation — failed on a CD-RW with:

```
Burn failed: The disc drive doesn't contain a disc.
```

The erase succeeded. The burn started immediately afterwards and found nothing.
The erase finishes before the *media* does: the framework returns, and the drive
has not yet re-read the disc.

The subprocess path never hit it. `drutil erase` and `drutil burn` were two
processes, and spawning the second took long enough for the drive to catch up.
Calling the framework in-process removed that accidental pause, so
`wait_for_blank_media` now makes it deliberate — poll the device until it
reports a blank disc, up to 30 seconds, then burn.

This is a product bug, not a test artifact. Linux keeps its existing behaviour:
`cdrskin blank=fast` is a separate process whose exit already means the drive is
ready.

### A data disc may be Mac-only, and it is an open question

A disc burned through `burn_data` has **no ISO 9660 Primary Volume Descriptor at
LBA 16**. A scan of the whole 1.26 MB found exactly one `CD001`, a type-255
terminator, at an offset that is not sector-aligned. If that reading is right,
the disc mounts on a Mac and nowhere else, while Linux writes ISO 9660 + Joliet
through `xorriso -joliet on`.

It is not a regression — `drutil` burned through this same framework with these
same defaults — and it is not settled either. The engine plainly plans an ISO
tree. `DRTrackEstimateLength` on one small folder, by filesystem mask:

| mask | blocks |
|---|---|
| default (all bits) | 648 |
| ISO 9660 + Joliet + HFS+ | 216 |
| HFS+ only | 189 |
| ISO 9660 + Joliet | 178 |
| ISO 9660 only | 173 |

Naming ISO 9660 + Joliet + HFS+ explicitly was tried as a fix and **reverted**:
every named set is a subset of the default, so pinning one can only take
filesystems off the disc. The default already asks for the widest set there is,
which makes "the default is wrong" the one explanation the numbers rule out.

So either the layout does not reach the media the way the estimate says, or
reading the session's LBA 0 through the whole-disc node does not land where
ISO 9660 counts from. The second looked like the better lead, and it was wrong.

**Settled 2026-09-04: the disc is Apple-only, and no node was being missed.**
`diskutil list` enumerates every partition on the medium, so it does not depend
on guessing which node to read. On a disc burned by `live_hw_rewrite_data` it
returns, in full:

```
0:  CD_partition_scheme                       *1.8 MB   disk12
1:  Apple_partition_scheme                     1.6 MB   disk12s1
2:  Apple_partition_map                       32.3 KB   disk12s1s1
3:  Apple_HFS  sparkamp-burn-13755           503.8 KB   disk12s1s2
```

That is the whole disc. There is no ISO 9660 partition at any node, at any
offset. `mount` confirms what carries the files: `/dev/disk12s1s2 on
/Volumes/sparkamp-burn-13755 (hfs, local, read-only)`, plain HFS rather than a
hybrid. So a data disc burned on macOS will not read on Linux or Windows, while
the same app on Linux writes ISO 9660 + Joliet through `xorriso -joliet on`.

**The explicit mask was then tried again, and it does not fix this.** Naming
`ISO 9660 + Joliet + HFS+` cut filesystem overhead from 547 blocks to 265 on the
same three files, so the mask does reach the burn. It added no ISO 9660. A scan
of every byte the drive reported written (539 blocks used, read through a
619-block node, so the scan covered all of it) found one `CD001`: a type-255
terminator at a non-sector-aligned offset, the same lone hit the original scan
found. A real descriptor set needs a type-1 primary at sector 16, and Joliet
would add a type-2 supplementary.

So the original revert reasoning was correct after all. The mask restricts and
cannot conjure a filesystem the engine is not generating, and the default was
never generating ISO 9660 either. The mask is kept regardless, because a third
fewer blocks is worth having.

What that actually costs is worth stating precisely, because "Mac only" is too
strong. Linux will usually mount an HFS+ disc, though the `hfsplus` module has
been orphaned since 2014, carried a "scheduled to be removed" deprecation
warning in 2025, and is commonly blacklisted for security. Windows reads neither
HFS+ nor an Apple Partition Map without third-party software. The case that
decides it for a music player is neither: a data CD of MP3s is most often played
by a car stereo or a DVD player, and those read ISO 9660 and Joliet only.

**Fixed and verified on hardware, 2026-09-04.** Sparkamp builds the ISO 9660 +
Joliet image itself, in `src/disc/iso9660.rs`, and burns it as a Mode 1 data
track through the same producer the CDDA path uses. `hdiutil makehybrid` would
have done it in one command and is unavailable, because the App Sandbox forbids
subprocesses.

The disc that came out mounts as `cd9660`, not `hfs`, and `diskutil` types the
partition `CD_ROM_Mode_1` rather than `Apple_HFS`. All three MP3s and the
playlist read back with hashes identical to their sources, and
`tone_440_copy2.mp3` keeps its lowercase name, which is Joliet doing its job:
ISO 9660 alone would show `TONE_440_COPY2.MP3;1`. The image is 450 blocks
against the 539 the HFS+ burn used for the same files.

Two things worth keeping from how this was chased. The filesystem mask is a red
herring in both directions: it does reach the engine, and it can only remove
filesystems, never add one. And LBA 16 of the whole-disc node is not LBA 16 of
the track on a CD, which is why the old check reported "no ISO 9660" on a disc
that had it. `live_verify_burned_data` now asserts the mounted filesystem name
through `statfs`, which is offset-independent and is the question that actually
matters.

`live_verify_burned_data` prints what it found and stays green; the measurement
is recorded rather than asserted, because the assertion belongs on the fix.

(`DRFSObjectGetFilesystemMask` cannot help settle it: it is exported, it links,
and calling it segfaults, the same rot as `DRCDTextBlockCreateArrayFromPackList`.)

The erase-first fix is verified on hardware: 72.7 s for erase plus burn in one
job, and the disc came back with **exactly the two new tracks**, not the three
it held before and not five. `live_verify_burned_data` checks that as an
equality rather than a count — the playlist must name exactly the audio files on
the disc — which is what distinguishes "replaced" from "appended" without the
test needing to know which burn it is looking at, and works for any burn.

### Three test defects this pass, all pre-existing

**`live_hw_burn_audio` asserted readback of a disc it had just ejected.** A burn
ends with an eject and always has — the pre-port code passed `-eject` too — so
the readback ran against media that had left the drive, and the test could not
have gone green on this hardware. Now split: `live_hw_burn_audio` writes and
waits for a reload, and `live_verify_burned_audio` does the readback alone. The
split matters more than the wait, because folded together a missed reload reads
as a failed burn, which is the opposite of what happened.

**`live_hw_erase` had the same defect**, for the same reason, and now shares the
same wait. The wait settles: a disc pushed back in reads as a data disc for a
second or two before its TOC is available, and returning that first probe made a
correct audio CD assert as `Data disc`. It waits for two agreeing reads, and
compares the media summary rather than the answer the caller is about to assert,
so the wait cannot decide the test.

**Its CD-TEXT assertion could never pass on macOS.** Not a port regression: the
pre-port code's own comment says `drutil` "carry no CD-TEXT regardless of
`sheet`". macOS has never written CD-TEXT, so DMG-vs-App-Store parity is intact
and the gap is macOS vs Linux. The assertion is now gated to Linux with that
reasoning in place rather than deleted.

That gap is now closed, and the assertion runs on both platforms — through each
one's own reader, because CD-TEXT only the burner can read is not a feature.
