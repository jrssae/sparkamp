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
