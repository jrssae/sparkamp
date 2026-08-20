# macOS Disc I/O Parity — closing a whole bug class, and making it testable

> **For agentic workers:** implement task-by-task. Steps use checkbox (`- [ ]`)
> syntax for tracking. Read the Audit first — it is the reason the plan is
> shaped this way.

**Goal:** stop macOS audio-CD tracks behaving as ordinary files to every
file-reading path in core, and build test coverage that catches this class on
Linux CI, where the bug is currently invisible by construction.

---

## Audit, 2026-08-20

Four defects were found and fixed in one session. They looked unrelated. They
were one bug.

| # | symptom | immediate cause | fixed |
|---|---|---|---|
| 1 | disc spins every 10 s, never settles | macOS poll ran `drutil status` (medium access) and discarded the cache | yes |
| 2 | poll fires mid-playback | guard raised only for `cdda://`, never for a file on a disc | yes |
| 3 | explicit Enqueue wiped the playlist | macOS honoured the add-behavior setting where GTK forces append | yes |
| 4 | ~1 s of audio, then high-speed thrash, app locked | now-playing probed a 40 MB AIFF **on the UI thread**, twice, per track change | yes |
| 5 | audio skips whenever a track is added while playing | `build_now_playing_info`'s **first line** — `id3_editor::read_tag_fields(path)` — parsed the whole 40 MB AIFF on the UI thread, every track change. Measured 2677 ms cold, 312–768 ms warm | yes |

Defect 5 is worth dwelling on: it took five wrong diagnoses to find, and every wrong one was
reasoned rather than measured. The theories that died — a probe storm, per-row `stat`s, a
missing `filename` index, cddafs syscall cost, a full SwiftUI list rebuild — were all
plausible and all wrong. What found it was timing instrumentation around one function.
`refreshPlaylist` never exceeded 20 ms; cddafs syscalls measure 3–53 **micro**seconds.

It also hid behind a partial fix of mine: gating the *artwork* fallback in the same function
made `rof.artwork_path` always empty, which moved the read rather than removing it. Gating one
call site in a function that reads the file three separate ways is not gating the function.

### The root asymmetry

`src/disc/toc.rs:4` — *"macOS uses the auto-mounted AIFF files, Linux uses
`cdda://` pseudo-URIs against the drive node."*

| | Linux audio-CD track | macOS audio-CD track |
|---|---|---|
| `Track.path` | `cdda://1?device=/dev/sr0` | `/Volumes/Audio CD/1 Audio Track.aiff` |
| `File::open(path)` | **fails instantly** | succeeds, reads optical media |

Every probe in core begins with `File::open`. On Linux an audio-CD track is
inert to all of them. On macOS it is a real, large, slow file.

Data discs mount as real files on **both** platforms, so they are symmetric.
The asymmetry is audio-CD-only, which is precisely why it hid for so long.

### Why GTK's testing cannot find this

GTK's `refresh_now_playing` fires from the tick loop **on every track change,
on the main thread** — structurally identical to macOS's `refreshNowPlaying`.
Both call `build_now_playing_info` → `read_only_track_fields` →
`probe_technical`. GTK never locks only because `File::open` rejects a
`cdda://` string in microseconds.

**GTK is the right source of truth for semantics and gives zero coverage of
this hazard class.** No amount of additional Linux disc testing will surface
it. That is the finding this plan exists to act on.

### The compounding factor (fixed)

`Cargo.toml` enabled symphonia's `mp3`, `isomp4`, `aac` — no AIFF. An `.aiff`
hint matched no registered reader, so symphonia fell through to the MP3
demuxer, whose `sync_frame` scans to EOF for a sync word PCM never contains.
Not a partial read: the entire file, every probe. `aiff` is now enabled, so any
remaining AIFF probe is a header read.

### Measured

- disc poll: **780 ms → 0.64 ms** once the devfs signature short-circuits it
- the hang: `sample` on the wedged process, **3728 of 3728** samples in
  `read()`/`readv()` under `probe_technical` → `MpaReader::try_new`

---

## Global Constraints

- **`cargo build && cargo test` must pass with zero warnings and zero
  failures** before any task is considered done. Current macOS baseline is 30
  warnings, all pre-existing dead-code on linux/GTK-only items.
- **Never `git push`.** Commit locally only; pushing needs a fresh instruction.
- **Ask before refactoring** beyond what a task specifies.
- **Deletion Rule** unchanged; nothing here deletes files.
- Tests that need hardware are `#[ignore]`d and named `live_*`, matching the
  existing convention.
- **Every task below must be verifiable on Linux CI.** A fix for this class
  that can only be checked on a Mac with a disc in the drive has not closed
  anything.

---

## Task 1: Make "this path is on a disc" a first-class fact

Today the only way to know is `detect::path_is_on_optical_media`, which
consults a mount list refreshed by polling. That is fine for the engine guard
but too weak to build enforcement on: it is empty until the first poll, and it
answers about *paths*, not about the track the user is playing.

- [ ] Add `Track::on_optical_media: bool`, set once where a disc track is
      built (`disc::toc::track_entries`) rather than inferred later. A fact
      known at construction should not be re-derived by every consumer.
- [ ] Keep `path_is_on_optical_media` for the paths that only have a string
      (the engine's `load`), and have it remain the fallback.
- [ ] `playlist_ingest::resolve` and `sparkamp_playlist_add_entry` must carry
      the flag through, or a disc track loses it the moment it enters a
      playlist — which is exactly when the probes start firing.

**Why a model field and not a helper call:** a helper is opt-in, and every
defect above came from a caller that did not know it had to opt in.

## Task 2: Make the probe boundary refuse disc reads by default

- [ ] `technical_probe::probe_technical`, `duration_probe::probe_duration`,
      `duration_probe::discover_duration` and `tags::read_track_tags` decline
      a path on optical media, returning their existing empty/default value.
- [ ] Add an explicit escape hatch — `*_allowing_disc` variants — for the
      callers that genuinely must read the disc: the ID3 editor (user opened a
      dialog and is waiting), `disc::rip`, `disc::cdtext`.
- [ ] Every escape-hatch caller must already hold the exclusive-read guard.
      Assert it in debug (`debug_assert!(exclusive_read())`), so a future
      caller that forgets is caught in tests rather than by a wedged drive.

**Result:** a new probe added anywhere in core is safe on discs without its
author knowing this document exists. That is the property being bought.

## Task 3: An I/O budget assertion for UI-thread paths

The now-playing bug would have been caught on Linux by a test that said "this
function opens no files". Make that expressible.

- [ ] Add a `#[cfg(test)]` counter incremented in each probe entry point
      (Task 2 gives one obvious place per probe).
- [ ] Expose `io_probe_count()` and a reset, test-only.
- [ ] Assert zero across the UI-refresh paths.

Named cases:

- [ ] `now_playing_info_opens_no_files` — build a `NowPlayingInfo` for a real
      on-disk file with no library row; assert the counter is zero. **Fails on
      today's `main`**, on Linux, with no hardware. That is the point.
- [ ] `now_playing_info_opens_no_files_for_a_library_row` — same for the
      indexed case, so the guarantee is not accidentally row-dependent.
- [ ] `id3_editor_fields_do_probe` — the inverse, pinning that the escape
      hatch still works and Task 2 did not silence everything.

## Task 4: Cross-platform parity tests for disc tracks

These run everywhere and encode the invariant rather than the platform.

- [ ] `a_disc_track_is_inert_to_every_probe` — construct a `Track` with
      `on_optical_media`, point it at a **real temp file with real audio**,
      call every probe, assert each returns its empty value and the Task 3
      counter stays zero. This is the Linux-runnable proxy for "a CD track on
      macOS".
- [ ] `disc_track_entries_flag_optical_on_both_platforms` — `track_entries`
      must set the flag whichever path shape the platform produces. Guards the
      `cfg` split in `toc.rs` directly.
- [ ] `an_ordinary_file_is_not_treated_as_optical` — the false-positive
      direction, so Task 2 cannot degrade into "probing is off".

## Task 5: Audit the remaining surface

Reached with an audio-CD track on macOS and **not yet verified**. Each needs a
test from Task 3/4, then a fix if it fails.

- [ ] **ReplayGain** — `rgAnalyzeMissing` over a disc track decodes the whole
      AIFF. Highest suspicion after now-playing.
- [ ] **`addFiles`** — fires `scan_metadata` + `probe_duration` per row. Now on
      the bounded pool, but still two full reads per CD track.
- [ ] **Artwork** — `read_track_tags(path).artwork_path` inside
      `read_only_track_fields`; Task 2 covers it, confirm.
- [ ] **Media library scan** — behaviour if a disc mount is inside a watched
      folder. Probably should refuse outright.
- [ ] **`sparkamp_playlist_add` / `add_fast`** — confirm neither probes.
- [ ] **Per-row status getters on the UI thread.** `sparkamp_playlist_file_missing`
      (`path.exists()`) and `sparkamp_playlist_is_read_only` (`access(2)`) are
      called once per row on every `refreshPlaylist()` — two syscalls per row,
      on the main thread. Guarded for optical paths (2026-08-20), but the
      general shape is still wrong: it is O(rows) filesystem calls per rebuild
      on **any** slow storage — a network mount, or simply a long playlist.
      GTK does not have this: the same markers come from the background
      `file_status` worker and only for rows on screen. Aligning macOS with
      that worker is the real fix, and it is a frontend change rather than a
      probe-boundary one.
      Note also that `sparkamp_playlist_add_entry` already stores
      `read_only: true` for disc rows and the getter was re-deriving it — a
      stored fact discarded and re-probed, which is the same mistake Task 1
      exists to stop.

## Task 6: Close the push/poll gap (separate, larger)

`subscribe_now_playing` is GTK-internal `AppState`, not core, so macOS
substitutes polling and says so: *"polling here on every track change is the
documented substitute."* Every macOS poll is a place GTK has an event, and each
was written without the guard GTK's equivalent has.

- [ ] Decide whether the FFI grows a callback/notification channel, or whether
      macOS keeps polling with mandatory guards.
- [ ] Not required to close this bug class — Tasks 1–5 do that. Recorded so it
      is not mistaken for done.

---

## Order, and why

| # | task | why here |
|---|---|---|
| 1 | optical fact on the model | everything else keys off it |
| 3 | I/O counter | needed to *prove* tasks 2 and 5, so build it early |
| 2 | probes refuse by default | the actual structural fix |
| 4 | parity tests | locks the invariant cross-platform |
| 5 | audit the rest | now cheap, because 3 makes each check one assertion |
| 6 | push/poll | independent, larger, optional |

Task 3 before Task 2 deliberately: write the failing test first, so the fix is
demonstrated rather than asserted.

## Already done (2026-08-20, uncommitted)

- devfs signature caching for the macOS drive poll (`disc/detect.rs`)
- exclusive-read guard raised for files on optical mounts (`engine.rs`)
- per-file FFI probes moved to the bounded pool (`ffi/playlist.rs`,
  `ffi/playback.rs`)
- `read_only_track_fields_no_probe`, used by `build_now_playing_info`
- symphonia `aiff` feature enabled
- macOS disc Enqueue passes `AddMode::Enqueue`; both Swift add-rule copies now
  call `sparkamp_should_replace_on_add`

## Deferred, deliberately

- **macOS has no test target at all** — one target, zero test files. Swift-side
  mistakes (a mode literal changed from `1` to `2`) are uncatchable. Worth its
  own decision.
- **`drutil status` cost is unmeasured when the drive is idle-but-loaded.** The
  devfs signature makes it moot in the common case; if a future change reopens
  it, measure before believing either way.
