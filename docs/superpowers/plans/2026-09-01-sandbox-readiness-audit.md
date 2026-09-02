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

## Not audited

Signing and provisioning (`Apple Development` today, App Store needs
`Apple Distribution` plus a matching profile), and the `export-options.plist`
method change. Those are packaging, not code, and are blocked on the bundle-ID
decision rather than on anything here.
