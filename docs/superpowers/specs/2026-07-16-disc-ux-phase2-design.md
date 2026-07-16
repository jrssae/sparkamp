# Disc UX Phase 2 — design

Date: 2026-07-16 · Status: approved by user · Branch: `burn-hardware-pass`
(continues the Send-to / per-drive-burn-queue work).

Five features that emerged from live GTK burn testing, designed together as
one "disc UX phase 2." All are additive; none change the Send-to or burn
behaviour already landed.

## Guiding decisions (user-confirmed)

- Scope: all five, one branch, sequenced tasks.
- #7 data-disc: **browse + play + add-to-library** (read-only disc; no
  writing back; stays in the Discs tab, not the Devices list).
- CD-TEXT edits: **per-drive, recompute defaults until overridden, cleared
  after a successful burn.**
- #6 progress: **determinate where possible** (prep i/N, burn % when the tool
  emits it), **animated/indeterminate** for erase and any phase without a
  percent; phase label above the bar.
- New #5 drag: **identical to Send-to ▸ Disc Drive** (probe-on-add,
  unreadable skipped + listed, live panel refresh); over-capacity still only
  blocks at burn time, not at drop.

## Shared seam

Extend `disc::burn::run_job`'s `phase: impl FnMut(&str)` to
`progress: impl FnMut(BurnProgress)` where

```rust
pub struct BurnProgress {
    pub label: String,        // "Erasing…", "Preparing 2/5 · <title>", "Burning…"
    pub fraction: Option<f32>, // Some(0.0..=1.0) determinate; None indeterminate
}
```

The GTK/TUI/mac callers translate it (label → status/overlay text, fraction →
progress bar). Backward-compatible in spirit: the string phases become
`BurnProgress { label, fraction }`.

Core-first throughout: pure logic in `src/`, unit-tested; frontends present.
mac Swift written blind + flagged for a Mac xcodebuild pass; TUI gets
capability parity only where it has the surface.

---

## A. New #1 — disc-swap auto-refresh (small)

**Problem:** swapping a disc in a drive doesn't refresh the open detail view;
the user must navigate away and back.

**Design:**
- Core: `detect::media_fingerprint(&OpticalDrive) -> u64` (or a small
  `MediaFingerprint` struct) hashing kind + is_blank + is_audio_cd +
  toc-track-count + capacity_bytes. Pure, unit-tested.
- GTK poll (`refresh_discs`): keep a per-drive fingerprint from the previous
  tick; when the **shown** drive's fingerprint changes, repopulate the detail
  view (`populate_disc_detail`) and the burn panel. Unchanged drives are
  untouched (the poll must not disturb selection — existing rule).
- Uses the borrow-snapshot discipline already applied to the poll (no borrow
  spanning `select_row`/`populate`).

**Tests:** fingerprint changes on kind/blank/track-count/capacity change,
stable otherwise.

---

## B. New #5 — drag files onto a disc-drive sidebar entry (small-med)

**Problem:** no drag path from files/playlist/active-playlist to a burner.

**Design:**
- Factor the current `ml.send-drive` async body into ONE shared helper
  (kills the present 4× duplication across files / editor / active-playlist /
  device views):

  ```rust
  fn queue_paths_to_drive(
      drive_id: String, drive_label: String,
      paths: Vec<PathBuf>,
      metas: HashMap<PathBuf, (String, Option<u32>, u64)>,
      burn_queues: Rc<RefCell<BurnQueues>>,
      burn_refresh_holder: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
      report: impl Fn(&AddOutcome, &str) + 'static, // status sink (label vs dialog)
      win_wk: glib::WeakRef<gtk4::Window>,
  )
  ```
  All four menu actions call it; so does the new drop handler.
- Add a `DropTarget` (`gdk::FileList`) to each disc-drive sidebar row
  (`disc:<id>`). On drop → build metas from the library (or filename
  fallback) → `queue_paths_to_drive`. Same probe-on-add + failure dialog +
  live refresh. No capacity gate at drop.

**Tests:** the shared helper's queue/skip/refresh path is exercised by
existing `add_files` unit tests; drop wiring is UI (interactive).

---

## C. CD-TEXT + editable disc metadata (med)

**Problem:** burned audio CDs show "Track 1/2/…" on hardware players; no way
to name the disc.

**Design:**
- Core `src/disc/cdtext.rs`:
  - `default_disc_meta(items: &[BurnItem]) -> DiscMeta` where
    `DiscMeta { artist: String, album: String }`; artist = the common track
    artist if all identical (parsed from `item.display`'s "Artist - Title"),
    else `"Various Artists"`; album = `format!("Sparkamp Disc {}",
    today_yyyy_mm_dd)`.
  - `build_v07t(meta: &DiscMeta, items: &[BurnItem]) -> String` — a Sony
    CD-TEXT v07t definition sheet: album title/performer + per-track
    title/performer. Track title/performer derive from `item.display`
    ("Artist - Title" split; whole string as title + disc artist as performer
    when no " - "). Pure; unit-tested byte-for-byte against a captured
    `cdtext_to_v07t` reference.
- `burn_audio`: write the v07t sheet into the staging dir and add
  `input_sheet_v07t=<path>` to the cdrskin args (before the WAV list).
  mac `drutil`: CD-TEXT via DiscRecording is not exposed by drutil — flag as
  a mac follow-up (drutil audio burns stay untitled for now; documented).
- `BurnList` gains `meta_override: Option<DiscMeta>`; the effective metadata =
  `meta_override` or `default_disc_meta(items)` recomputed each render.
  Cleared (set to `None`) after a successful burn along with the queue clear.
- GTK burn panel (audio mode only): two `Entry`s — "Disc artist" and "Disc
  album" — prefilled with the effective defaults, updating as the queue
  changes UNTIL the user types (then `meta_override` holds their value).
  Feeds `build_v07t` at burn time.

**Tests:** `default_disc_meta` (all-same → artist; mixed → Various Artists;
empty), `build_v07t` sheet format, override-vs-default selection.
Live `--ignored`: burn with metadata, `cdtext_to_v07t` readback asserts the
album + a track title.

---

## D. #6 — burn progress overlay (med)

**Problem:** "Erasing…"/burn show static text with no activity; looks hung
(a real drive-timeout looked like a freeze during hardware testing).

**Design:**
- Core: `run_job` emits `BurnProgress` (shared seam above).
  - Prep: `fraction = Some((i as f32 + within) / n)` — `within` from
    `prepare_wav` if it reports position, else per-track steps.
  - Erase: `fraction = None` (indeterminate).
  - Burn: parse the tool's progress. `run_tool` today captures stdout to a
    log file; change it to **stream** the child's stdout line-by-line
    (still teeing to the log for the error tail) and hand a parsed fraction
    up via a callback. Add `parse_cdrskin_progress(line) -> Option<f32>`
    (e.g. "Track 01: 12 of 34 MB written" → 12/34), pure + unit-tested.
    macOS `drutil`: `parse_drutil_burn_progress` stub already noted; keep
    indeterminate if no percent lines.
- GTK: a `gtk4::Overlay` over the disc detail content. During a burn the
  overlay shows a phase label + `ProgressBar` (determinate when
  `fraction.is_some()`, else `pulse()` on a timer). Burn progress state lives
  in shared app state **keyed by drive id**, so navigating away and back to
  that drive re-shows the live overlay; it clears on Done/Failed/Cancelled.
- The existing Cancel button drives the same cancel path.

**Tests:** `parse_cdrskin_progress` (valid/invalid/edge), prep-fraction math.
Interactive: overlay shows, updates, survives navigation, clears on finish.

---

## E. #7 — data-disc browsing + add-to-library (larger)

**Problem:** a burned/data MP3 disc shows nothing browsable; the user expects
to see and play its files.

**Design:**
- Core `src/disc/mount.rs`:
  - `ensure_mounted(drive: &OpticalDrive) -> Result<PathBuf, String>` — if the
    disc's block device is already mounted, return its path; else mount
    read-only via the udisks D-Bus client the devices layer already uses.
    Never writes.
  - `list_disc_files(mount: &Path) -> Vec<DiscFile>` where `DiscFile { path,
    display, duration_secs, bytes }` — walk the mount, audio files only, read
    tags (reuse `devices::browse::read_device_track` / the same tag path).
- GTK: when the loaded media is a data disc (not audio CD, present), the disc
  detail shows a **file list** (mirroring the device track view): double-click
  / Enter plays; Send-to menu works (queue to another drive, add to playlist);
  and a **"Add to Library"** button/action copying the selected files into
  the library music folder (reuses `add_files_to_library`), then rescans.
  Read-only — no writing to the disc. Lives in the Discs tab; the disc is NOT
  added to the Devices list (optical-excluded rule preserved).
- Contention: browsing reads the disc; guard with `set_exclusive_read` like
  the other disc reads so it doesn't fight a cdda stream / rip / burn.

**Tests:** `list_disc_files` filters + tag-fill (against a temp dir standing
in for a mount). Live `--ignored`: mount the burned data disc, assert the
three test files appear. Interactive: play + add-to-library.

---

## Parity

- **mac (blind):** BurnProgress overlay, CD-TEXT (drutil gap flagged), data-
  disc browse+add-to-library, drag-to-drive, auto-refresh — mirror structure,
  flag for a Mac xcodebuild + manual pass.
- **TUI:** BurnProgress in the burn overlay (determinate/indeterminate line);
  CD-TEXT editable fields in the burn view; auto-refresh on media change.
  Data-disc browsing + drag-to-drive are GTK/mac surfaces the TUI lacks —
  flagged out of scope (the TUI has no device/drag surface today).

## Pre-work (user-approved 2026-07-16, from the branch UX audit)

**P0 — media_library.rs page split: DEFERRED (2026-07-16).** The intended
mechanical split is impossible — `include!` in a fn body splices a single
expression, not statements, and macro hygiene hides `let` bindings. A real
split is an interface rewrite (context struct + per-page builder fns);
user deferred it to a dedicated branch after phase 2. Boundary map kept in
`.superpowers/sdd/task-1-report.md`.

**P1 — Send-to consistency fixes** (fold into / alongside task B's
`queue_paths_to_drive` dedupe):
- G1: the files-view "Send to ▾" button acted on the last RIGHT-CLICKED
  selection (`ml_selected_tracks` fills only in the context-menu gesture);
  its Active Playlist entry used the live selection. Unify: every Send-to
  entry (menu and button) reads the LIVE selection at dispatch time.
- G2: the editor's button-row "Send to…" (whole playlist + .m3u8 → device)
  becomes the standard full "Send to ▾" menu on selected tracks, with the
  whole-playlist sender preserved as an extra entry in that menu
  ("Entire playlist to device ▸" — files + .m3u8 semantics unchanged).
- G3: success feedback normalized to a QUIET status line in every view —
  add small status labels to the editor page and device view; interim
  "Reading files…" everywhere; dialogs only for unreadable-file failures.
- G4: empty selection → status "Select tracks first" instead of a silent
  no-op.
- Cleanup: drop the editor device-popover's `connect_closed(unparent)`
  (pattern that broke action dispatch elsewhere; harmless here but
  divergent).

## Sequencing

P0 (file split) → P1 + B (consistency + drag-to-drive + send-drive dedupe,
one motion) → A (auto-refresh) → C (CD-TEXT) and D (progress overlay) in
parallel (independent) → E (data-disc browsing).
Each task: build + test in dev-box, zero warnings, commit.

## Out of scope

- Writing/appending to an existing data disc from the browse view.
- Multisession burning (unchanged deferral).
- Mac verification (separate Mac session).
- Adding optical discs to the Devices/sync list.
