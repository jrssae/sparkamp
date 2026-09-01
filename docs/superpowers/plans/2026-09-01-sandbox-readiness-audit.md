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

App Sandbox blocks `posix_spawn` of anything outside the app bundle. Every
macOS-reachable spawn in the tree is `/usr/bin/drutil`.

| Call site | What it does |
|---|---|
| `src/disc/cdtext.rs:263` | reads CD-TEXT (`CDTEXT_TOOL` is `drutil` on macOS, `cdrskin` on Linux) |
| `src/disc/detect.rs:827` | the shared `run()` helper, drive detection and status |
| `src/disc/detect.rs:1003` | eject |
| `src/disc/burn.rs:385` | burning an audio CD |

`src/display_backend.rs` also spawns, five times, but every one is
`#[cfg(target_os = "linux")]`. Not a macOS concern.

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

## Open questions for a human

1. Does the App Store build ship optical disc support at all? Section 1 is
   large and this is the cheapest way to make it disappear.
2. Do the DMG and App Store builds coexist on one machine? Determines whether
   section 4's migration copies or moves.
3. Bundle identifier: `com.sparkamp.sparkampmac` is in the Xcode project,
   `CLAUDE.md` names `dev.sparkamp.Sparkamp` for the Linux/Flatpak side. Pick
   one on purpose before registering in App Store Connect.

## Not audited

Signing and provisioning (`Apple Development` today, App Store needs
`Apple Distribution` plus a matching profile), and the `export-options.plist`
method change. Those are packaging, not code, and are blocked on the bundle-ID
decision rather than on anything here.
