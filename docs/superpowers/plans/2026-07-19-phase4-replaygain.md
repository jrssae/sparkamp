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

## Pre-written pipeline-rebuild code (2026-07-19, written against engine.rs @ 4535875)

The highest-risk chunk, frozen while the code could be read carefully.
Verify anchors at execution; the DESIGN is binding even if lines drifted.

**Key insight that de-risks everything:** the pipeline is static
(built once in `Player::new()`, chain `audioconvert → [spectrum] →
volume → [equalizer] → sink`), and `load()` sets the pipeline to
`gst::State::Null` on EVERY track change (engine.rs:494). Null is the
safe relink window — so ReplayGain inserts/removes elements ONLY at
Null, and NO dynamic pad-blocking surgery exists anywhere in this
design. Config changes mid-track defer to the next `load()`; changes
while Stopped apply immediately. (Deferred-apply is also Winamp's
behavior and avoids the entire class of PLAYING-state relink bugs.)

Target chain when active:
`audioconvert → [spectrum] → rgvolume → [rglimiter] → volume → [eq] → sink`
(rgvolume BEFORE Sparkamp's own volume/preamp so user volume stacks on
top of normalization; rgvolume's own `pre-amp` property stays 0.)

`album-mode` is a live-settable gboolean — it is NOT chain shape and
never triggers a rebuild.

```rust
// ── config.rs ──────────────────────────────────────────────────────────
// (playback.replaygain group; serde defaults per the settings section)

// ── engine.rs additions ────────────────────────────────────────────────

/// The chain-shape subset of the ReplayGain config: the two flags that
/// decide WHICH elements sit in the pipeline. `album-mode` and the
/// fallback gain are live properties, deliberately not part of this
/// struct — changing them must not trigger a rebuild.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RgChain {
    pub enabled: bool,
    pub clip_protection: bool,
    pub fallback_db: f64, // applied at build; live-settable later too
}

// New Player fields:
//   rg_volume: Option<gst::Element>,    // in-chain rgvolume, when active
//   rg_limiter: Option<gst::Element>,   // in-chain rglimiter, when active
//   rg_applied: RgChain,                // shape currently IN the pipeline
//   rg_pending: Option<RgChain>,        // desired shape awaiting a Null window
//   rg_album_mode: bool,                // last-set album-mode (re-applied on rebuild)
// Initialize in new(): rg_volume/rg_limiter = None,
//   rg_applied = RgChain { enabled: false, clip_protection: false, fallback_db: 0.0 },
//   rg_pending = None, rg_album_mode = false.
// new() itself is UNCHANGED — the chain starts exactly as today and the
// first apply happens via set_replaygain (config load) before first play.

impl Player {
    /// True when the GStreamer rgvolume element is installed (rglimiter
    /// ships in the same plugin). Feature silently no-ops without it.
    pub fn rg_available() -> bool {
        gst::ElementFactory::find("rgvolume").is_some()
    }

    /// The element the RG segment hangs off: spectrum when present,
    /// else audioconvert (mirrors the link logic in new()).
    fn rg_upstream(&self) -> &gst::Element {
        self.spectrum_elem.as_ref().unwrap_or(&self.audioconvert)
    }

    /// Request a ReplayGain chain shape. Applies immediately when the
    /// pipeline is Null (Stopped); otherwise defers to the next load()
    /// — mid-track toggles take effect on the next track by design.
    pub fn set_replaygain(&mut self, cfg: RgChain) {
        if cfg == self.rg_applied {
            self.rg_pending = None;
            return;
        }
        if self.state == PlayerState::Stopped {
            // stop()/pre-first-load pipelines are already Null; the extra
            // set_state is belt-and-suspenders and a no-op when so.
            let _ = self.pipeline.set_state(gst::State::Null);
            let _ = self.apply_rg_chain(cfg);
        } else {
            self.rg_pending = Some(cfg);
        }
    }

    /// Live album/track-mode switch (Automatic source sets this at each
    /// track start from the shuffle state). Never rebuilds the chain.
    pub fn set_rg_album_mode(&mut self, album: bool) {
        self.rg_album_mode = album;
        if let Some(ref rgv) = self.rg_volume {
            rgv.set_property("album-mode", album);
        }
    }

    /// Rebuild the RG segment. CALLER CONTRACT: pipeline state is Null.
    /// Never call from Playing/Paused — that is what rg_pending is for.
    fn apply_rg_chain(&mut self, cfg: RgChain) -> Result<()> {
        // ── 1. Tear out whatever RG segment is currently linked. ──
        // Clone the upstream handle so &mut self borrows don't fight.
        let upstream = self.rg_upstream().clone();
        if let Some(rgv) = self.rg_volume.take() {
            upstream.unlink(&rgv);
            if let Some(rgl) = self.rg_limiter.take() {
                rgv.unlink(&rgl);
                rgl.unlink(&self.volume_elem);
                self.pipeline.remove(&rgl)?;
            } else {
                rgv.unlink(&self.volume_elem);
            }
            self.pipeline.remove(&rgv)?;
        } else {
            // Today's direct link (also the disabled shape).
            upstream.unlink(&self.volume_elem);
        }

        // ── 2. Build the requested segment. ──
        if cfg.enabled {
            if let Ok(rgv) = gst::ElementFactory::make("rgvolume")
                .name("rgvol")
                .build()
            {
                rgv.set_property("fallback-gain", cfg.fallback_db);
                rgv.set_property("album-mode", self.rg_album_mode);
                // Sparkamp's preamp lives on its own volume element; keep
                // rgvolume's internal pre-amp at its 0.0 default.
                self.pipeline.add(&rgv)?;
                upstream.link(&rgv)?;

                let tail = if cfg.clip_protection {
                    match gst::ElementFactory::make("rglimiter").name("rglim").build() {
                        Ok(rgl) => {
                            self.pipeline.add(&rgl)?;
                            rgv.link(&rgl)?;
                            self.rg_limiter = Some(rgl.clone());
                            rgl
                        }
                        // Limiter missing but rgvolume present: degrade to
                        // gain-without-limiting rather than no RG at all.
                        Err(_) => rgv.clone(),
                    }
                } else {
                    rgv.clone()
                };
                tail.link(&self.volume_elem)?;
                self.rg_volume = Some(rgv);
                self.rg_applied = RgChain {
                    clip_protection: self.rg_limiter.is_some(),
                    ..cfg
                };
                return Ok(());
            }
            // rgvolume missing entirely → fall through to the direct link
            // (house rule: silent no-op when plugins are absent).
        }

        upstream.link(&self.volume_elem)?;
        self.rg_applied = RgChain {
            enabled: false,
            clip_protection: false,
            fallback_db: cfg.fallback_db,
        };
        Ok(())
    }
}

// ── load() integration (engine.rs:490-494) ────────────────────────────
// Immediately after `self.pipeline.set_state(gst::State::Null)?;` add:
//
//     // The Null window is the only safe moment to reshape the RG
//     // segment; a config change made mid-track lands here.
//     if let Some(cfg) = self.rg_pending.take() {
//         let _ = self.apply_rg_chain(cfg);
//     }

// ── Controller call points ────────────────────────────────────────────
// 1. Startup + settings change: build RgChain from
//    config.playback.replaygain and call player.set_replaygain(chain).
// 2. Track start (phase-2 seam): for Source::Automatic call
//    player.set_rg_album_mode(!shuffle_enabled); for Track/Album call
//    with false/true respectively. Fallback-gain slider changes can
//    set_property live via a small setter mirroring set_rg_album_mode,
//    or ride the same set_replaygain path (shape-equal → deferred no-op
//    — note the early return means fallback_db-only changes need the
//    live setter; add `set_rg_fallback_db(&mut self, db)` mirroring
//    set_rg_album_mode).
```

**Pre-written tests** (dev-box has gst-plugins-good; guard each with
`if !Player::rg_available() { return; }` so plugin-less environments
skip rather than fail):

```rust
#[cfg(test)]
mod rg_tests {
    use super::*;

    fn player() -> Player {
        gst::init().unwrap();
        Player::new().unwrap()
    }

    // Peer-check helper: element A's src pad must feed element B's sink.
    fn feeds(a: &gst::Element, b: &gst::Element) -> bool {
        a.static_pad("src")
            .and_then(|p| p.peer())
            .map(|peer| peer.parent_element().as_ref() == Some(b))
            .unwrap_or(false)
    }

    #[test]
    fn rg_chain_full_shape() {
        if !Player::rg_available() { return; }
        let mut p = player();
        p.set_replaygain(RgChain { enabled: true, clip_protection: true, fallback_db: -6.0 });
        let rgv = p.pipeline.by_name("rgvol").expect("rgvolume inserted");
        let rgl = p.pipeline.by_name("rglim").expect("rglimiter inserted");
        assert!(feeds(&rgv, &rgl));
        assert!(feeds(&rgl, &p.volume_elem));
        assert_eq!(rgv.property::<f64>("fallback-gain"), -6.0);
    }

    #[test]
    fn rg_chain_no_limiter_shape() {
        if !Player::rg_available() { return; }
        let mut p = player();
        p.set_replaygain(RgChain { enabled: true, clip_protection: false, fallback_db: 0.0 });
        let rgv = p.pipeline.by_name("rgvol").expect("rgvolume inserted");
        assert!(p.pipeline.by_name("rglim").is_none());
        assert!(feeds(&rgv, &p.volume_elem));
    }

    #[test]
    fn rg_disable_restores_direct_link() {
        if !Player::rg_available() { return; }
        let mut p = player();
        p.set_replaygain(RgChain { enabled: true, clip_protection: true, fallback_db: -6.0 });
        p.set_replaygain(RgChain { enabled: false, clip_protection: false, fallback_db: -6.0 });
        assert!(p.pipeline.by_name("rgvol").is_none());
        assert!(p.pipeline.by_name("rglim").is_none());
        // upstream feeds volume directly again (spectrum absent in test
        // builds? it isn't — spectrum is built in tests; use rg_upstream).
        let up = p.rg_upstream().clone();
        assert!(feeds(&up, &p.volume_elem));
    }

    #[test]
    fn rg_mid_play_change_defers_to_load() {
        if !Player::rg_available() { return; }
        let mut p = player();
        p.set_state_for_test(PlayerState::Playing);
        p.set_replaygain(RgChain { enabled: true, clip_protection: true, fallback_db: -6.0 });
        assert!(p.pipeline.by_name("rgvol").is_none(), "must not relink while playing");
        p.set_state_for_test(PlayerState::Stopped);
        p.load("file:///nonexistent.mp3").unwrap(); // Null window applies pending
        assert!(p.pipeline.by_name("rgvol").is_some());
    }

    #[test]
    fn rg_album_mode_is_live_no_rebuild() {
        if !Player::rg_available() { return; }
        let mut p = player();
        p.set_replaygain(RgChain { enabled: true, clip_protection: false, fallback_db: 0.0 });
        let rgv = p.pipeline.by_name("rgvol").unwrap();
        p.set_rg_album_mode(true);
        assert!(rgv.property::<bool>("album-mode"));
        p.set_rg_album_mode(false);
        assert!(!rgv.property::<bool>("album-mode"));
    }
}
```

Notes for the implementer:
- The EQ element is `None` in `#[cfg(test)]` builds (engine.rs:196-197)
  but spectrum IS built in tests — `rg_upstream()` handles both.
- Tests construct real pipelines: they need gstreamer initialized and
  the dev-box plugin set; the `rg_available()` guard keeps them honest
  elsewhere. Follow the existing live-test idiom if `gst::init()` in
  multiple tests races (it's idempotent; it doesn't).
- `by_name` on the pipeline is the assertion seam — no pad probes needed.
- fallback_db-only changes: add `set_rg_fallback_db` live setter (one
  liner mirroring set_rg_album_mode) — see controller call point 2.
- Do NOT touch the pad-added callback, the cdda handling, or the
  waveform probe — the RG segment is entirely between upstream and
  volume_elem.

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
