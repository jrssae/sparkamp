# Arena synthesis: the `AudioBackend` seam

> Written during the architect fan-out that produced the `AudioBackend` seam,
> 31 August 2026, and moved into the repository on 2 September because it lived
> in a session scratchpad that was about to be discarded. It is the reasoning
> behind `2026-08-31-audio-backend-seam-design.md`, which is the design of
> record.
>
> Four candidate designs were commissioned in parallel and judged against each
> other. References to "opus", "sonnet", "haiku" and "fable" are those
> candidates, and "the judge" is the cross-model review that ranked them.

## Base

**Candidate opus.** My Phase D pick and the cross-judge's recommendation agree,
which arena treats as confirmation rather than as two opinions.

Judge scores against the six-criterion rubric: opus 29/30, haiku 15/30.

## Field, including dropouts

| Runner | Outcome |
|---|---|
| opus | complete, base |
| haiku | complete, weaker, two grafts taken |
| sonnet | killed at 33 min. Transcript stalled at 554 KB with no output written. |
| fable | never ran. HTTP 429, "Fable 5 requires usage credits". |

Two of four runners produced nothing. The arena ran at half strength and the
synthesis should be read with that in mind.

## Verification performed by the parent, not delegated

Trust artifacts, not self-reports. Both candidates' summaries were wrong about
their own work.

- **opus compiles.** Stub harness for `anyhow`, `crate::model`, `crate::granite`.
  Zero errors.
- **The coexistence claim holds.** I wrote the test rather than believing the
  claim. `Player::new()`, `Player::<NullBackend>::open()`, and a bare
  `struct Ctx { _player: Player }` field all compile in one binary. The runtime
  panic is `not implemented` at `design.rs:901`, which is the intended stub.
- **opus's citations into the real tree are accurate.** Spot-checked five.
  `engine.rs:358` is the `#[cfg(not(test))]` pad probe. `model.rs:1170` and
  `1185` carry `#[cfg_attr(test, allow(dead_code))]`. `model.rs:1112` reallocs on
  band-count change. `Player::rg_available()` has exactly one caller, and opus
  correctly distinguished it from the nine hits on
  `crate::replaygain::rg_analysis_available()`, a different function.
- **haiku does not compile.** `error[E0425]: cannot find type Mutex`. Used at
  line 236, only `Arc, RwLock` imported at line 11. Fails identically under
  `cargo build` and `cargo test --no-run`.
- **haiku's method count is wrong in its own summary.** Claimed 18, defines 15.

Corrections to my own earlier reporting, recorded because getting this wrong in
a synthesis note is worse than getting it wrong out loud:

- I claimed haiku's `AudioBackendType` was an unguarded duplicate. False. Both
  non-test arms carry `#[cfg(not(test))]`. The judge caught my misreading. The
  file still does not compile, for the unrelated `Mutex` reason above.
- The judge's single strongest argument against the base is the `rg_available`
  signature change, contingent on its "exactly one caller" claim being true. I
  had already verified that claim empirically. **That risk is retired.**

## Grafts taken

**G1. Sink injection into `GstBackend`.** From Josef's decision D1, not from a
candidate. `Player` hardcodes `autoaudiosink` at `engine.rs:238`. The GStreamer
adapter takes the sink as a construction parameter so a test can pass
`fakesink sync=false`, which reaches PLAYING with a real decode and no audio
device. Verified with `gst-launch-1.0`. This is what lets
`rg_mid_play_change_defers_to_load` assert against real pipeline state instead of
a field somebody wrote to. See `DECISIONS.md`.

**G2. The App Store argument for compile-time backend selection.** From haiku's
rationale, which reasons that a macOS bundle carrying GStreamer and its plugins
invites rejection. That is a sharper, more concrete justification for rejecting
`Box<dyn>` than anything in opus's own alternatives section, and it happens to be
the actual reason this project exists. Folds into opus's "Alternatives
considered" as supporting evidence.

**G3. The state-consistency open question.** From haiku. Should `Player`
reconcile its own `state` shadow with the backend's real state during a
transient or degraded-backend error? opus's design shrinks the surface for this
but does not answer it. Added to opus's open questions.

## Rejections, and why

The rejection notes are the highest-signal part of this record.

- **haiku's whole selection mechanism.** `AudioBackendType` resolves to
  `NullBackend` for any `cfg(test)` build, so no test can reach a real
  `equalizer-10bands` through `Player` at all. The literal 8-line stub goes away
  and the underlying problem relocates rather than resolves. `live_cdda_tests`,
  which the grounding names as wanting the real backend, could not run through
  `Player` in a test build under this design.
- **haiku's objection to generics** ("monomorphization bloats binaries; Linux and
  macOS builds compile both"). Simply incorrect for the base design. `avf` is
  `#[cfg(target_os = "macos")]`-gated, so each platform binary monomorphizes
  exactly one `DefaultBackend`.
- **haiku's `fn state(&self) -> PlayerState` on the trait.** Duplicates the
  `state` field `Player` already owns above the seam, with nothing naming which
  is authoritative.
- **Merging `set_output_gain`, `set_eq`, `set_normalization` into one
  `set_audio_chain`.** opus considered and rejected this; I agree and am
  recording it because 10 to 8 methods looks like depth. They change at three
  different rates. Gain moves every tick during a fade, EQ at slider rate,
  normalization at track boundaries. Merging re-pushes a ten-band curve on every
  fade tick and forces the adapter to diff on the hottest path.

## What the base actually buys

Stated plainly, because "we added a trait" is not a benefit.

The seam is a **net subtraction**. It deletes four `#[cfg(test)]`-shaped
workarounds that all share one cause: every test built a real GStreamer
pipeline, so the EQ element had to be neutered for all of them at once.

| Workaround | Location |
|---|---|
| EQ element forced to `None` | `engine.rs:276-283` |
| waveform pad probe compiled out | `engine.rs:358` |
| `WaveformBuffer::capacity` dead-code allow | `model.rs:1170` |
| `WaveformBuffer::push_samples` dead-code allow | `model.rs:1185` |

It also collapses two records of one guard count (`cdda_device` behind an
`Arc<Mutex<_>>` plus `holds_disc_guard: AtomicBool`) into a single `bool`, and
deletes `rg_applied`, a hand-synced shadow of pipeline shape.

`Player` goes from 26 fields to 19, none of them audio-stack types.

And it closes a leak I had misattributed. I had ReplayGain splitting cleanly at
policy versus mechanism. It does not: GStreamer's "relink only at `State::Null`"
rule currently lives in `Player`, so `Player` is already contaminated. Pushing a
complete `Normalization` down and having the adapter answer
`Applied::{Now, AtNextLoad}` means the reload dance in `ffi_apply_replaygain`
stops firing on macOS without any caller changing.

## Costs accepted

- Nine supporting types. Six are trivial newtypes or enums; each has a citable
  job. The judge argued this earns its place and I agree, with `Capabilities`
  (three bools) the weakest of the nine.
- `Player` becomes generic. Turbofish in some tests, a second monomorphization in
  the test binary, slower test compiles.
- One signature genuinely changes. `Player::rg_available()` moves from an
  associated function to `&self`. One caller, a test helper at `engine.rs:1714`.
  Verified.
- The design rests on a default type parameter, which is an advanced idiom. A
  maintainer who does not recognise it might "simplify" it back into the
  crate-wide `cfg(test)` trap. This wants a comment at the declaration saying
  why, not just what.

## Verification still owed

opus claims it compiled copies of the real FFI call sites. I verified the
type-level design in a stub harness; I did not verify those specific call sites.
That check belongs in Phase F of implementation, against the real crate.
