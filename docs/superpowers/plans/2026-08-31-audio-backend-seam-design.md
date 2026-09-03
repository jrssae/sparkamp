# AudioBackend seam: the design of record

> **For agentic workers:** this is the design the implementation must match. It
> is the output of a parallel design exploration, not one agent's opinion. Read
> the spike (`2026-08-31-macos-audio-backend-spike.md`) first; three of its
> measurements are load-bearing here.

**Decision: put the seam *under* `Player`, not at it.** `Player` stays one
concrete struct holding a generic backend parameter with a `cfg`-selected default
type argument. Ten trait methods. Two adapters on day one.

---

> The reasoning that produced this design is in three companion documents,
> moved out of a session scratchpad on 2026-09-02:
> `2026-08-31-audio-backend-arena-grounding.md` (the brief the candidates were
> given), `-synthesis.md` (which candidate won and which parts of the others
> were grafted in), and `-decisions.md` (the open questions settled during
> synthesis).

## Why a seam at all

The honest objection first, because it nearly killed this design. Adding a trait
with one implementation is a hypothetical seam, and "it enables AVFoundation
later" is speculation dressed as justification.

What settles it is that the seam is a **net subtraction**. Four
`#[cfg(test)]`-shaped workarounds in the tree today share one cause: every test
builds a real GStreamer pipeline, so the EQ element has to be neutralised for all
of them at once.

| Workaround | Location |
|---|---|
| EQ element forced to `None` in tests | `src/engine.rs:276-283` |
| waveform pad probe compiled out | `src/engine.rs:358` |
| `WaveformBuffer::capacity` dead-code allow | `src/model.rs:1170` |
| `WaveformBuffer::push_samples` dead-code allow | `src/model.rs:1185` |

They delete together. A test that does not want GStreamer asks for
`NullBackend`, instead of asking for a crippled GStreamer.

`Player` also loses two records of one guard count (`cdda_device` behind an
`Arc<Mutex<_>>`, plus `holds_disc_guard: AtomicBool`, with a prose comment
explaining that only one may be set at a time) and `rg_applied`, a hand-synced
shadow of pipeline shape. 26 fields become 19, none of them audio-stack types.

## The shape

```rust
pub struct Player<B: AudioBackend = DefaultBackend> { … }

#[cfg(all(target_os = "macos", feature = "avf"))]
pub type DefaultBackend = avf::AvBackend;
#[cfg(not(all(target_os = "macos", feature = "avf")))]
pub type DefaultBackend = gst::GstBackend;
```

**`#[cfg]` picks the default; the constructor picks the actual.** That sentence
is what the design turns on.

The default type argument means bare `Player` in a type position still resolves,
so `src/ffi/mod.rs:68`'s `player: Player`, `Controller`'s `&'a mut Player`, and
`prime_player_gain` need no edit. `new()` is defined **only** on
`impl Player<DefaultBackend>`, so `Player::new()` resolves with no annotation. A
`new()` on the generic impl would make every bare call ambiguous, so that split
is load-bearing.

Tests then select per test, in one binary:

| Test | Constructs |
|---|---|
| `eq_volume_pin` | `Player::<NullBackend>::open()` |
| ReplayGain element tests | `GstBackend::open(analysis.tap(), sink)` |
| `live_cdda_tests` | `Player::new()`, unchanged |

`Box<dyn AudioBackend>` is rejected: it advertises a runtime backend swap the
system does not have, and someone would eventually add `set_backend()`. The
trait is `: Sized` and `open` returns `Self`, so that door is shut by the type
system rather than by a comment. A macOS bundle carrying GStreamer and its
plugin set also invites App Store rejection, which is the concrete reason
compile-time selection is right here and not merely tidier.

A plain `cfg`-selected concrete type is rejected because it is the trap: a null
backend needs a third `cfg` arm, and `cfg` arms are crate-wide, so no test could
reach a real `equalizer-10bands` through `Player` at all.

## The trait

Ten methods, partitioned by knowledge owned rather than by execution order.

```rust
fn open(tap: AnalysisTap) -> Result<Self>;
fn capabilities(&self) -> Capabilities;
fn load(&mut self, source: &MediaSource) -> Result<()>;
fn set_state(&mut self, state: PlayerState) -> Result<()>;
fn seek(&mut self, to: Duration) -> Result<()>;
fn timeline(&self) -> Timeline;
fn poll_event(&mut self) -> Option<BusEvent>;
fn set_output_gain(&mut self, gain: Amplitude);
fn set_eq(&mut self, curve: &EqCurve);
fn set_normalization(&mut self, want: Normalization) -> Applied;
```

Where the collapses came from. `play`/`toggle_pause`/`stop` become one
`set_state`, because the transition table is `Player` policy and a backend has no
opinion on what a toggle means. `has_eq`/`has_spectrum`/`rg_available` become one
`capabilities()`, read once, since it cannot change after `open`.
`position`+`duration` become one `timeline()`, because `sparkamp_tick` reads them
five times per tick and quietly assumes they agree; under AVFoundation both come
off one `sampleTime` snapshot, so one struct makes that assumption true rather
than lucky. Spectrum and waveform become **zero** methods; capture crosses once
at `open`.

`set_output_gain`, `set_eq`, and `set_normalization` are deliberately **not**
merged into one `set_audio_chain`, even though 10 to 8 looks like depth. They
change at three different rates: gain every tick during a fade, EQ at slider
rate, normalization at track boundaries. Merging re-pushes a ten-band curve on
every fade tick and forces the adapter to diff on the hottest path.

## What stays above the seam

Written once in `Player`, never per backend: the granite visualizer, the fadeout
ramp, `stop_after_current`, the display-band frequency table and waveform
resampling, and the output composition rule
`user_volume * user_preamp * fade_factor`. Roughly half of `Player`'s ~48 methods
never touch an audio stack, and duplicating them per backend is the failure mode
this shape exists to avoid.

## ReplayGain splits at *when* versus *how*

The line falls in a different place than it looks. Today `Player` owns
GStreamer's "relink only at `State::Null`" rule: `set_replaygain` decides
immediate versus deferred and `rg_pending` carries the deferral. **That is
GStreamer's relink rule leaking upward**, so `Player` is already contaminated.

Under the seam `Player` always pushes the complete desired `Normalization` and
the adapter answers `Applied::{Now, AtNextLoad}`. GStreamer answers `AtNextLoad`
for an enable or clip-protection change mid-track and `Now` for album-mode or
fallback. A gain-node backend answers `Now` always, and the reload dance in
`ffi_apply_replaygain` stops firing on macOS with no caller changing.

The five existing `engine::rg_tests` assert element graph shape through private
fields. Four move down one level intact into `engine::gst`, `Player` swapped for
`GstBackend`, same helpers and assertions.
`rg_mid_play_change_defers_to_load` splits: the policy half moves to `engine`
over `NullBackend` and gets **stronger**, since today all five silently `return`
when `rgvolume` is absent and the deferral rule is untested in any environment
without the plugin.

### The GStreamer half of that test

**Decided: assert against real pipeline state.** Not an `#[ignore]`d live test.

Today the test never plays anything. `set_state_for_test(PlayerState::Playing)`
writes `Player`'s own field, `set_replaygain` consults that field, and the load
step uses `file:///nonexistent.mp3`. `engine.rs:1601` already admits in a comment
that an empty pipeline cannot reach Playing. Under the seam the adapter reads its
own real state, so the lie stops working.

Two measurements make the decision cheap. **No CI runs the suite**:
`gtk-check.yml` runs `cargo check --all-targets --locked`, which compiles tests
and never executes them. And **a real pipeline reaches PLAYING without audio
hardware**, verified with `gst-launch-1.0`:

```
uridecodebin ! audioconvert ! volume ! equalizer-10bands ! fakesink sync=false
```

This is why `GstBackend` takes its sink as a construction parameter rather than
hardcoding `autoaudiosink` as `engine.rs:238` does. The test generates a short
silent WAV itself; no binary fixture gets committed. The assertion stays honest,
because the state read is the pipeline's.

## Analysis capture is an inverted handle, not a getter

`Player` owns `Analysis` (read half) and hands `AnalysisTap` (write half,
`Clone + Send + Sync`) to the backend at `open`. The GStreamer pad probe and the
AVFoundation tap block each hold a clone and write from their own thread. The
trait has zero spectrum or waveform methods.

The spike's advisory-buffer-size finding costs this nothing, and not by luck:
`push_pcm` and `push_magnitudes_db` take any slice length, and both `crate::model`
types already absorb that, since `SpectrumData::update` reallocates its smoothing
array to whatever band count it receives (`src/model.rs:1112`). No consumer at
any level states a capture size, so 4410 where 1024 was asked for is not an
event. An adapter must **not** buffer up to a "correct" size, because there is
none.

`push_magnitudes_db` takes dB rather than linear magnitude, since that is the
unit the normalization is defined in and the unit GStreamer's `spectrum` element
emits. AVFoundation has no spectrum element, so its adapter runs a vDSP FFT and
converts before pushing, and the two then produce bars that look the same rather
than bars that look plausible.

The display-band mapping stays in `Player`. Duplicating the frequency table per
adapter is exactly how the two platforms end up looking different.

## The -3.01 dB pan law

A contract stated on the trait and honoured in the adapter. `Amplitude::UNITY`
means the same audible level on every platform, which is what GStreamer's
`volume` element passes at 1.0. An output stage with attenuation of its own must
cancel it. Not a `fn output_gain_offset_db()` on the trait, which would put an
AVFoundation detail in shared vocabulary. Not silent in the adapter either, or
the next adapter author reproduces the bug.

## Invariants encoded in types

| Type | Mistake it prevents |
|---|---|
| `Amplitude` | writing dB where a linear multiplier is expected, which is exactly the -3.01 dB finding |
| `EqCurve` | an unclamped band reaching the backend; the clamp moves into the constructor |
| `Normalization` + `Applied` | `Player` shadowing pipeline shape, and the relink rule leaking upward |
| `MediaSource` | a CD device travelling out-of-band through `Arc<Mutex<Option<String>>>` |
| `Timeline` | position and duration read from different instants |
| `Capabilities` | three capability probes drifting apart |
| `Analysis` / `AnalysisTap` | an adapter growing its own presentation policy |
| `AudioBackend: Sized` | a future `Box<dyn>` and a runtime backend swap |

## Costs accepted

- Nine supporting types. Six are trivial newtypes or enums. `Capabilities`
  (three bools) is the weakest of them.
- `Player` becomes generic: turbofish in some tests, a second monomorphization in
  the test binary, slower test compiles.
- **One signature genuinely changes.** `Player::rg_available()` moves from an
  associated function to `&self`. It has exactly one caller, a test helper at
  `engine.rs:1714`. Verified: the nine other `rg_available` hits in the tree are
  `crate::replaygain::rg_analysis_available()`, a different function.
- The design rests on a default type parameter, an advanced idiom. A maintainer
  who does not recognise it might "simplify" it back into the crate-wide
  `cfg(test)` trap. The declaration needs a comment saying *why*, not just what.
- `Controller` stays pinned to `DefaultBackend`, so its eight tests are untouched
  by this change. Making it generic is a one-line follow-up if they later want
  `NullBackend`.

## Open questions

- Should `AnalysisTap` be two handles (`PcmTap`, `MagnitudeTap`) so "these never
  interact" lives in the types rather than a doc comment? On GStreamer the two
  writers are genuinely different threads.
- `Amplitude` is a newtype while dB stays a bare `f64` named `*_db`. Is that
  asymmetry right, given the -3.01 dB bug was a unit confusion?
- Should `Player` reconcile its `state` shadow with the backend's real state
  during a transient or degraded-backend error? The seam shrinks this surface
  without closing it.
- `MediaSource::CdTrack` is GStreamer-only and the AVFoundation adapter will
  error on it. macOS reaches audio CDs as mounted files so it should never fire,
  but that is inference from `live_cdda_tests`, not measurement.

## Provenance

Parallel design exploration, four runners, one candidate per model. Two produced
nothing: one stalled and was killed at 33 minutes, one had no credits available.
The surviving two were scored against a six-criterion rubric by an independent
cross-judge, which reached the same base independently: 29/30 against 15/30.

The losing candidate still contributed the App Store argument for compile-time
selection and the state-consistency open question above.

Both candidates' self-descriptions were wrong about their own work, so every
claim in this document that cites a file and line was checked against the tree by
hand, and the winning sketch was compiled in a stub harness before being adopted.

## Next step

Write `src/engine/backend.rs`: the trait and all nine supporting types with
`unimplemented!()` bodies, plus `EqCurve`'s and `Amplitude`'s own unit tests.
Land it as a compiling commit with `Player` untouched, so the vocabulary is
reviewable before any GStreamer code moves.

---

## The switch, thrown (2026-09-02)

`DefaultBackend` is `avf::AvBackend` on macOS. One line, which is what the seam
was for.

### Formats: a gain, not a trade

The worry was that AVFoundation would decode less than GStreamer. Measured, by
loading one file of each through the adapter and pulling audio until frames come
out with signal in them — "it opened the file" is not the bar:

| | decodes |
|---|---|
| shipped GStreamer bundle | mp3, flac, ogg, opus, wav |
| AVFoundation | mp3, flac, ogg, opus, wav, **aac, m4a, aiff** |

The bundle's list is not a guess: `packaging/macos/build-dmg.sh` copies an
explicit plugin allowlist, and it carries no AAC decoder, no AIFF parser and
nothing for wma, ape, mpc, tta or wavpack. So the switch **adds** three formats
and removes none.

`AUDIO_EXTENSIONS` claims fourteen. Five of them — wma, ape, mpc, tta, wv — play
on neither backend as macOS ships them. That gap between what the list claims
and what the platform delivers predates this work and is untouched by it.

`avf_decodes_the_shipped_formats` is the measurement, kept.

### What the switch found

Flipping the default surfaced a real gap the adapter had shipped with, dormant:
**`AvBackend` held no exclusive-read guard.**

`GstBackend` raises `begin_exclusive_read` for a `cdda://` stream *or* a file on
a mounted optical volume, and macOS reaches an audio CD entirely through the
second path — one mounted AIFF per track. Without it the ten-second drive poll
reads the medium underneath playback, which is the hazard the guard exists for
and which this codebase has already been bitten by once.

Ported, with one difference that is its own bug. `AvBackend::set_state` returns
early when the state is unchanged, and `load` leaves the state at `Stopped` — so
a stop after a load released nothing, and the guard stayed up until the backend
was dropped. The release now happens **before** that early return.

### Why the unit test cannot cover this on macOS

`path_is_on_optical_media` answers from `statfs` for any path that can be
stat'd, and falls back to the polled mount list only for one that cannot. A real
temp directory therefore reports `apfs` and can never stand in for an optical
mount on macOS — the existing unit test reached the seeded list only because its
paths did not exist.

That test is now named `GstBackend` explicitly, since a lazily-loading backend
is what makes a non-existent path loadable at all, and it covers the decision
and the balancing. The macOS shape is `live_avf_disc_playback_holds_the_guard`,
run against a real audio CD.

### What it costs

Recorded on `AvBackend::set_normalization` and unchanged by this switch:
ReplayGain applies as a plain gain, `clip_protection` has no limiter behind it,
and `album_mode` has nothing to choose between because `AVAudioFile` does not
surface a stream's own REPLAYGAIN tags. It is now shipping behaviour on macOS
rather than dormant code, which raises its priority without changing what it is.
