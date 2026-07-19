# Phase 4 — F7 ReplayGain (design-level plan)

> Read `2026-07-19-opus-handoff.md` first. Biggest engine phase; todo marks
> it "hugely important". Settings values below are user-decided — verbatim.

**Goal:** Full Winamp-style ReplayGain: playback gain via GStreamer
`rgvolume` (+`rglimiter`), analysis via `rganalysis` into the DB (+optional
tag write-back), six settings, library actions + opt-in column.

## Architecture

### Playback (engine.rs)

- Pipeline today: playbin → volume (pre-amp) → equalizer-10bands. Insert
  `rgvolume` (and `rglimiter` when clipping protection on) BEFORE EQ/volume
  in the audio-filter bin. `rgvolume` reads REPLAYGAIN_* tags off the
  stream automatically.
- Element properties mapping (config → rgvolume):
  - master toggle OFF → bypass (remove elements from bin or set
    `album-mode`-independent passthrough; simplest robust approach: build
    the filter chain per configuration at pipeline (re)build, matching how
    EQ elements are added today).
  - Source: Track / Album / Automatic → `album-mode` bool; Automatic =
    decide at track start: album-mode when advancing sequentially
    (shuffle off), track-mode when shuffling. Needs a track-start hook —
    reuse phase 2's seam.
  - "Adjustment for files WITHOUT RG info": `fallback-gain` dB (-12..0,
    default -6, 0 = off).
  - Clipping protection ON (default) → `rglimiter` after rgvolume; also
    `rgvolume::pre-amp` stays 0 (keep Sparkamp's own pre-amp separate).
- Missing plugins (rgvolume/rglimiter/rganalysis are in
  gst-plugins-good/-base) → silent no-op per house rule; probe with
  `ElementFactory::find` once, log, disable the feature flags.

### Analysis (new `src/replaygain.rs`)

- Analysis pipeline per batch: `filesrc ! decodebin ! audioconvert !
  audioresample ! rganalysis num-tracks=N ! fakesink`, reading tag events
  (`replaygain-track-gain/peak`, album gain/peak on batch end). Batch =
  album group (tracks sharing (album, album_artist-or-artist)) so album
  gain is meaningful; singletons analyze alone.
- DB always: new `new_cols` — `rg_track_gain REAL, rg_track_peak REAL,
  rg_album_gain REAL, rg_album_peak REAL` (+ LibTrack fields, 5 SELECTs,
  mapper — same drill as phase 1; REMEMBER both sort paths if the column
  is sortable).
- Tag write-back toggle (default OFF): ID3v2 TXXX frames
  `REPLAYGAIN_TRACK_GAIN` ("-6.20 dB" format), `_TRACK_PEAK` ("0.988123"),
  `_ALBUM_GAIN`, `_ALBUM_PEAK` via id3 crate ExtendedText. Non-MP3 formats:
  skip write-back (id3-only path today) — known limitation entry.
- Job runner: background thread, cancelable, progress via the existing
  scan-status channel pattern; queue of album batches; skip tracks with
  values unless "re-analyze" requested.

### Settings (all three frontends; config `playback.replaygain` group)

1. `enabled: bool = true` — "Use ReplayGain"
2. `source: Track|Album|Automatic = Automatic` (dropdown)
3. `clip_protection: bool = true`
4. `fallback_db: f32 = -6.0` (slider -12..0, 0 = off)
Media Library tab:
5. `auto_analyze: bool` — analyze on add/scan (background, cancelable)
6. `write_tags: bool = false`

### Library UI

- Files-view context action "Calculate ReplayGain" (selection → batch).
- Bulk "Analyze missing ReplayGain" button near Rescan; progress in the
  scan status area.
- Opt-in ML column `rg_gain` showing track gain dB (e.g. "-6.2 dB");
  both sort paths.
- mac: settings rows (existing FFI settings pattern:
  `sparkamp_get/set_*` per field — 6 pairs + bridge.h), context action,
  column. TUI: settings_eq-adjacent settings screen entries where the TUI
  settings surface reaches.

## Automated tests

- Config plumbing: defaults, serde roundtrip.
- Automatic-source decision fn: (shuffle on → track), (sequential → album)
  — pure, table-tested.
- Album batching: grouping fn over LibTrack sets (album_artist fallback,
  singletons, empty album → per-track batch).
- TXXX write/read roundtrip: write fields via the write-back fn, read with
  id3 crate, assert exact frame descriptions + value formats; verify other
  frames preserved.
- dB formatting fns ("-6.20 dB" exact Winamp-compatible format).
- Pipeline construction: unit-test the element-chain builder returns the
  expected element names per config combo, gated on plugin availability
  (skip-if-missing with `#[ignore]`-style runtime check like the existing
  gstreamer-dependent tests — see how engine tests handle missing plugins).
- Analysis end-to-end: generate a WAV (tests already have a generator),
  run the analysis pipeline, assert gain/peak values come back finite and
  stored — mark ignored if the plugin's absent in CI-ish envs; dev-box has
  gst-plugins-good.

## Manual test plan

1. Two loud/quiet tracks with RG tags: audible leveling with master ON,
   raw difference with OFF.
2. Source Track vs Album on an album with quiet interludes; Automatic
   switches behavior when shuffle toggles.
3. Untagged file: fallback slider audibly attenuates (-6 default), 0
   disables.
4. Clipping protection: hot RG-tagged file (+gain) doesn't clip with
   limiter on.
5. Bulk analyze on a folder: progress shown, cancel works, values appear
   in the opt-in column; re-run skips analyzed.
6. Write-back ON: analyze → open the file in another player (or
   `eyeD3`/`id3v2 -l`) → REPLAYGAIN_* TXXX present; write-back OFF →
   file untouched (mtime check).
7. Context "Calculate ReplayGain" on a selection.
8. mac: settings persist + affect playback; column; Xcode checklist.

## Performance notes

- rganalysis DECODES ENTIRE FILES — 36k-track full analysis is hours.
  Never auto-trigger a full-library job; `auto_analyze` applies to newly
  added/scanned files only. Bulk action is explicit, progress-visible,
  cancelable, and resumable (skip rows with values).
- Serialize analysis (one pipeline at a time) — decoding is CPU-bound and
  parallel pipelines thrash; keep the queue single-worker.
- Write-back mutates files → triggers phase-8 watcher events later; use
  the same self-write suppression noted in the phase-8 doc.

## Open questions

1. Non-MP3 write-back (FLAC/OGG native RG tags) — skip (recommended,
   id3-only today) or add via symphonia-adjacent writer? Propose skip +
   known limitation.
2. Re-analyze semantics for the bulk button: "missing only" (recommended)
   with a modifier/second action for "force re-analyze selection"?
