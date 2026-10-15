# Handoff: check the macOS side for what the Linux review found

Written on Linux (oscar) on 15 October 2026, by the agent that reviewed the
`cross-linux-and-minor-fixes` branch, moved the GTK build to GNOME 51 and
fixed what the review turned up. You are on a Mac. Two jobs: make sure none
of that broke the macOS build, and look for the same kinds of problem in the
Swift frontend and the macOS-only Rust, which nothing on Linux can compile.

Report what you find before changing anything structural. CLAUDE.md asks for
that ("Ask before refactoring"), and the user decides which findings get
fixed. Bugs with an obvious, local fix are the exception only if the user
says so.

---

## Do this first

The branch is `cross-linux-and-minor-fixes`. When this was written its new
commits were **not pushed**; if `git log` on your side stops at
`8a0b5c9 fix(mpris): …`, ask the user to push before you go further.

```bash
cargo vendor
```

`Cargo.lock` changed (gtk-rs 0.11, libadwaita 0.9, gstreamer 0.25). None of
those are built on macOS, but `vendor/` must match the lock for anything to
resolve.

---

## 1. Prove the macOS build still builds

Nothing below has been compiled for macOS. These changes touch shared code
and need a Mac to confirm:

- **Eleven `#![allow(dead_code)]` were removed** from core modules (`dedupe`,
  `skin`, `watch`, and `devices::{browse, sync, diagnostics, plan, io,
  marker, transfer, detect}`), plus one on `impl Queue`. On Linux that left
  zero warnings, and a scan found no non-public item in those modules used
  only by Linux-gated code, so macOS should see none either. That was
  reasoning, not a build.
- **`cargo clippy --fix` rewrote shared code** in about 90 files: mostly
  let-chains, plus `repeat_n`, `is_multiple_of`, `as_chunks`,
  `io::Error::other`. Everything needs Rust 1.88 or newer.
- **`# Safety` sections** were added to the public FFI functions in
  `src/ffi/mod.rs`, and several doc comments moved.

Run, with your normal toolchain:

```bash
RUSTFLAGS="-D warnings" cargo build --workspace
RUSTFLAGS="-D warnings" cargo test --workspace
RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets
```

then the app build (Xcode, or `bash packaging/macos/build-dmg.sh`).

**Clippy has never been run on macOS-only code.** CI lints on Linux only
(`.github/workflows/gtk-check.yml`), so `src/engine/avf.rs`,
`src/disc/discrecording.rs`, `src/disc/transcode/avf.rs`, the
`cfg(target_os = "macos")` arms elsewhere and `frontends/macos` are unlinted.
Expect findings. The project's policy is in `[lints.clippy]` in Cargo.toml:
four lints are off crate-wide, each with its reason. A lint that is right in
general but wrong at one site gets `#[expect(clippy::…, reason = "…")]` there.
Fix the rest. If a dead-code warning appears, prefer deleting dead code over
allowing it, and say which.

---

## 2. The review findings, and where macOS stands

For each: what was wrong on Linux, what reading the code says about macOS,
and what to check.

### Next under Repeat Song replayed the song (GTK) — fixed

GTK carried its own copy of the navigation logic instead of calling
`sparkamp::controller::Controller`, and the copy treated Repeat Song as "stay
on this track" for a manual skip. Commit `418a367` puts GTK on the
controller.

macOS calls the controller through `sparkamp_nav_next` / `sparkamp_nav_prev`
(`SparkampModel+Transport.swift`), and the remote commands
(`SparkampModel+NowPlaying.swift`) go through the same `next()` / `prev()`.
**Check** that every other route does too: keyboard shortcuts
(`SparkampModel+Keys.swift`), menu commands (`SparkampMacApp.swift`) and the
Touch Bar (`TouchBarControls.swift`). Any route that moves the playlist
itself is the GTK bug again.

### Back after a restart recorded the track twice (GTK) — fixed

A restart (Back more than 5 s in) is meant to leave the shuffle history alone.
GTK re-recorded the track, so the next quick Back restarted it again.
**Check** that no Swift path restarts a track with a call that records into
shuffle history; the controller's `nav_prev` plus a no-record play is the
correct pairing.

### Background tags written to the wrong row (GTK, TUI) — fixed

Add Folder wrote tags read in the background to `scan_start + index`, so
deleting a row mid-scan shifted every later title onto its neighbour. Core
now has `Playlist::apply_scanned_tags` (matches by `Track::id`) and a single
ordered `ScanEvent` channel. The macOS bridge already keyed its metadata
reads by id. **Check** the rest of the Swift side for the same shape:
anything that captures a row index, awaits or dispatches, then writes back by
that index. Duration probes and row-status updates are the likely places.

### Saves are not atomic — not fixed, the user is deciding

`Config::save`, `Playlist::save_last` and the `.m3u8` writers truncate and
then write. A full disk empties the file. On Linux an unreadable config stops
the app at startup. **On macOS, `src/ffi/mod.rs` loads it with
`Config::load().unwrap_or_default()`, so a damaged config silently becomes
defaults and the next save makes that permanent.** Worth stating plainly in
your report, since the user's decision on atomic writes may depend on it.
Swift's own writes (`Theme.swift`) already use `atomically: true`.

### Deleting device files stripped playlist entries for failed deletes — fixed

`devices::plan::device_delete_files` (reached through
`sparkamp_device_delete_files`) now removes playlist entries only for files
it actually deleted. **Check** the macOS device view: a delete that partly
fails should say so and leave those entries in place. Entries are still
matched by filename, as before.

### Symlink loops multiplied or hung folder walks — fixed

`Playlist::collect_audio_files` and `MediaLibrary::walk_dir` now skip a
directory they have already entered (by canonical path). Both are reached
from macOS through the FFI. **Check** for Swift-side recursive walks. A
search found only `Theme.swift`'s non-recursive skin listing, but look at
drag-and-drop folder expansion and anything using
`FileManager.enumerator(at:)`.

### Visualizer click did nothing while stopped (GTK) — fixed

macOS needs nothing here: `VizClickCatcher` always calls `cycleVizMode()`.
Confirm by eye if convenient.

### Rip filenames could carry a NUL — fixed

`pathutil::safe_file_name` is now the only filename sanitiser, and it
replaces control characters. `disc::rip::dest_path` uses it, macOS rips
included. **Check** whether Swift builds any filename from tag or disc data
itself (save panels, exports) and routes it through the same rule or one of
its own.

---

## 3. Complexity to look for on the Mac side

The Linux fixes, for comparison: GTK reimplemented controller logic; the same
action had separately written copies for buttons, keys and MPRIS; blanket
lint allows hid real problems; and a few functions held thousands of lines in
one scope. Look for the macOS versions of those.

- **Swift reimplementing core decisions.** Shuffle, repeat, queue order,
  playlist sort or search filtering done in Swift rather than through the FFI.
  The Media Library column table (`MLFilesTable.ColumnSpec`) is a known,
  accepted duplicate (docs/refactor-plan.md, Phase 4); leave it.
- **One action, several copies.** For each transport and mode action, list
  the routes (view button, keyboard, menu, Touch Bar, remote command) and
  confirm they all call one model method. Drift between copies is how GTK's
  `b`/`z` keys came to skip the metadata scan.
- **Blanket allows in macOS-only Rust.** `src/engine/avf.rs` keeps
  `#![allow(dead_code)]`, because its comment says the AVFoundation backend
  is "tested but nothing in the app constructs it yet". CLAUDE.md says macOS
  plays through AVFoundation. One of those is out of date: find out which,
  and if the backend is live, remove the allow and see what is really dead.
  Fifteen `src/ffi/*.rs` modules open with
  `#![allow(unsafe_op_in_unsafe_fn)]`. Report whether that is still the right
  call rather than changing it.
- **Large views holding a lot of shared state.** By line count:
  `DiscDriveView.swift` (1,463), `MediaLibraryWindow.swift` (1,172),
  `SparkampModel+Discs.swift` (1,119), `Id3EditorWindow.swift` (1,101),
  `PlayerWindow.swift` (1,017). Length alone is not the problem;
  docs/gtk-breakup-plan.md §9 explains why. Look for views whose
  `@State`/`@Binding` set is read from far apart within one body, which is
  the SwiftUI form of the capture web that made the GTK pages hard to follow.

---

## 4. Don't

- Add GStreamer to any macOS code path.
- Touch the bundle ID (`com.sparkamp.sparkampmac`); see CLAUDE.md.
- Start a release. That takes two separate approvals from the user.
