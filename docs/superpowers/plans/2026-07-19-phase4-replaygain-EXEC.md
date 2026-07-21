# Phase 4 — F7 ReplayGain (execution plan)

Expanded from `2026-07-19-phase4-replaygain.md` (design doc — READ IT, esp. the
pre-written pipeline-rebuild code + tests, which are binding). Read the handoff
first. Branch `album-art-improvements`. Base at start: `36c64c4` (phase-3
pushed). Suite floor: 461 lib + 665 bin, 0 warnings.

## Resolved decisions (user, 2026-07-21)
- **Tag write-back = MP3 ONLY** via the existing `id3` crate (TXXX
  REPLAYGAIN_*). NO new dependency (lofty rejected). M4A/WMA/FLAC/OGG/WAV get
  DB values (column/display/playback-if-already-tagged) but NO tag write-back —
  known limitation. Default write_tags = false.
- **Bulk "Analyze ReplayGain" = MISSING OR STALE**: a track qualifies when it
  has no stored `rg_track_gain` OR its `file_mtime` is newer than `last_scanned`
  (reuse the phase-1 mtime/last_scanned columns). Plus a Files-view context
  action "Force re-analyze" that recomputes the selection regardless.
- Playback source default = Automatic; fallback_db default = -6.0; enabled
  default = true; clip_protection default = true (all per the design doc).
- Design doc's Null-window pipeline-rebuild approach is BINDING (no dynamic
  pad-blocking; inserts/removes at `gst::State::Null` only; mid-track changes
  defer to next `load()`).

## Global rules
- Build/test ONLY in distrobox. 0 warnings. GTK bin-only; new src/ modules →
  `mod` in BOTH lib.rs AND main.rs. rganalysis/rgvolume/rglimiter live in
  gst-plugins-good/-base (present on dev-box); guard tests with
  `if !Player::rg_available() { return; }` and feature-detect at runtime
  (silent no-op when absent — house rule).
- Analysis DECODES WHOLE FILES: single-worker queue, cancelable, progress via
  the existing scan-status channel; NEVER auto-run a full-library job
  (auto_analyze = newly added/scanned only).
- Config: `#[serde(default)]` + Default. Borrow discipline. No push without ask.

## Tasks

### P4-T1 — Config + pure decision fns (core, TDD)
- `config.rs`: add `ReplayGainConfig` under `playback` (group `replaygain`):
  `enabled: bool=true`, `source: RgSource=Automatic` (enum Track/Album/Automatic,
  serde), `clip_protection: bool=true`, `fallback_db: f32=-6.0`,
  `auto_analyze: bool=false`, `write_tags: bool=false`. `#[serde(default)]` +
  Default impl. Tests: defaults, serde roundtrip.
- Pure decision fn `rg_album_mode(source: RgSource, shuffle_enabled: bool) -> bool`
  (Track→false, Album→true, Automatic→!shuffle). Table test. Put in a small
  core module or config.rs.
Files: src/config.rs (+ maybe src/replaygain.rs stub). Small.

### P4-T2 — Engine pipeline (rgvolume/rglimiter) — THE pre-written code
Implement the design doc's pre-written block VERBATIM (verify anchors at
`engine.rs`): `RgChain` struct; Player fields (rg_volume/rg_limiter/rg_applied/
rg_pending/rg_album_mode); `rg_available`, `rg_upstream`, `set_replaygain`,
`set_rg_album_mode`, `set_rg_fallback_db` (live setter, one-liner), `apply_rg_chain`;
`load()` integration (apply rg_pending at the Null set_state, engine.rs ~:490-494).
Add the 5 pre-written `rg_tests` (guarded by rg_available). Do NOT touch the
pad-added callback / cdda / waveform probe. Gate: build + the rg_tests pass on
dev-box.
Files: src/engine.rs. HIGHEST RISK — follow the frozen code + tests exactly.

### P4-T3 — DB columns + LibTrack (core, TDD)
Mirror the phase-1 column drill: add `rg_track_gain REAL, rg_track_peak REAL,
rg_album_gain REAL, rg_album_peak REAL` to the tracks table (migration/new_cols
add-if-missing), + LibTrack fields, + the ~5 SELECT sites, + the row mapper, +
`upsert`/write path for RG values (a dedicated `set_replaygain(id, tg, tp, ag, ap)`
or extend upsert). Sortable `rg_gain` → BOTH sort paths (ml_columns.rs ml_sort_key
+ queries.rs ORDER BY map) if the column is sortable. Tests: write→read RG values
roundtrip; a track with no RG → NULLs.
Files: src/media_library/{mod.rs,queries.rs,scan.rs?}, ml_columns.rs. Follow
phase-1 pattern (P1 tasks in the ledger).

### P4-T4 — Analysis core (new src/replaygain.rs, TDD)
- Album batching fn: group `LibTrack`s into batches sharing (album,
  album_artist-or-artist); empty album → per-track batch; singleton → alone.
  Pure, table-tested.
- dB/peak formatting fns: `format_gain_db(f32) -> "-6.20 dB"`,
  `format_peak(f32) -> "0.988123"` (Winamp-compatible). Tested exact.
- Analysis pipeline per batch: `filesrc ! decodebin ! audioconvert !
  audioresample ! rganalysis num-tracks=N ! fakesink`, read tag events
  (replaygain-track-gain/peak per track; album gain/peak on batch end). Return
  per-track + album results.
- Job runner: single-worker background thread, cancelable (AtomicBool), progress
  via the scan-status channel pattern; queue of album batches; skip rows unless
  forced; the "missing OR stale (mtime>last_scanned)" filter for the bulk job.
- End-to-end test: generate a WAV (existing generator), run analysis, assert
  finite gain/peak (guard on rg_available/plugin present; #[ignore] if absent).
Files: src/replaygain.rs (+ mod in lib.rs & main.rs).

### P4-T5 — MP3 tag write-back (core, TDD)
`id3`-based writer: write TXXX ExtendedText frames `REPLAYGAIN_TRACK_GAIN`
("-6.20 dB"), `_TRACK_PEAK` ("0.988123"), `_ALBUM_GAIN`, `_ALBUM_PEAK`;
preserve other frames. MP3 only — non-MP3 path returns Ok(skipped)/logs (known
limitation). Reader for verification. Tests: write→read roundtrip (exact frame
descriptions + formats), other frames preserved, non-MP3 skip. Gate default
write_tags=false: only invoked when the setting is on.
Files: src/replaygain.rs or src/id3_editor.rs (TXXX helpers). Update spec
known-limitations for non-MP3 write-back.

### P4-T6 — GTK: settings + library UI
- Settings: playback tab rows — Use ReplayGain (enabled), Source dropdown
  (Track/Album/Automatic), Clipping protection, Fallback slider (-12..0, 0=off);
  Media Library tab — Analyze on add/scan (auto_analyze), Write tags (write_tags).
  Persist via the tab's save idiom.
- Files-view context: "Calculate ReplayGain" (selection → force batch),
  "Force re-analyze" (or fold: single action = force on selection).
- Bulk "Analyze missing ReplayGain" button near Rescan → missing-or-stale job;
  progress + cancel in the scan-status area.
- Opt-in ML column `rg_gain` ("-6.2 dB" from rg_track_gain); both sort paths.
- Skin: any new widgets get selectors + covers test if applicable.
Files: frontends/gtk/window/{settings.rs,media_library.rs,ml_columns.rs}.

### P4-T7 — TUI: settings entries
ReplayGain settings where the TUI settings surface reaches (settings_eq-adjacent):
enabled/source/clip/fallback + auto_analyze/write_tags as toggles/cycles. Keyboard
walk. (Analysis job trigger in TUI optional — note capability.)
Files: frontends/tui/settings_eq.rs (+ ui). 

### P4-T8 — mac (BLIND): settings FFI + column + context
6 FFI get/set pairs (`sparkamp_get/set_rg_*`) + bridge.h mirrored; SparkampLibTrack
gains rg_track_gain (+ maybe the others) for the column; settings rows in mac
Settings; ML art-style column; context "Calculate ReplayGain" → an analysis FFI
trigger (`sparkamp_rg_analyze_selection`/`_missing` + progress poll) OR reuse the
scan-progress channel. mac-pass-checklist. Gate = Rust suite. Read whole files.
Files: src/ffi/*, bridge.h, frontends/SparkampMac/*.

### P4-T9 — Controller wiring (GTK + core seam)
- Startup + any RG-settings change: build RgChain from config.playback.replaygain,
  call player.set_replaygain(chain); fallback_db-only change → set_rg_fallback_db.
- Track start (phase-2 now-playing seam / play_and_update): for Source::Automatic
  call player.set_rg_album_mode(!shuffle_enabled); Track→false, Album→true.
- auto_analyze: on add/scan of NEW files, enqueue analysis (single-worker).
Files: frontends/gtk/window/{player.rs, settings.rs}. Small but load-bearing.

### P4-T10 — Phase close-out
Full gate; final whole-branch review (most-capable model) over the phase-4 diff;
ONE fix subagent + re-review; spec known-limitations (non-MP3 write-back, whole-
file decode cost, mid-track deferred-apply); ledger; mac checklist; user manual
test list (the design doc's 8-item plan). NO push without ask.
