# Grounding: extracting an `AudioBackend` seam from Sparkamp's `Player`

> Written during the architect fan-out that produced the `AudioBackend` seam,
> 31 August 2026, and moved into the repository on 2 September because it lived
> in a session scratchpad that was about to be discarded. It is the reasoning
> behind `2026-08-31-audio-backend-seam-design.md`, which is the design of
> record.
>
> Four candidate designs were commissioned in parallel and judged against each
> other. References to "opus", "sonnet", "haiku" and "fable" are those
> candidates, and "the judge" is the cross-model review that ranked them.

Repo: `/Users/josefschelch/Code/Sparkamp`, branch `app-store-compatibility`.
Rust audio player (Winamp-style). Read `src/engine.rs` in full before designing.

## The task

`src/engine.rs` defines one concrete struct `Player` (~1817 lines, ~48 public
methods) whose fields are GStreamer elements. We need a seam under `Player` so a
second audio backend (AVFoundation, for the Mac App Store) can replace GStreamer
without `Player`'s callers changing.

**Design the seam.** Produce the trait, the types it needs, the revised `Player`
field set, and the module map. Do not write the implementations.

## Hard constraints

1. **`Player`'s public API must not change.** `src/ffi/mod.rs:68` holds
   `player: Player` and the FFI layer calls ~30 of its methods across
   `src/ffi/*.rs`. Swift calls into Rust through that FFI. Changing `Player`'s
   surface means changing the Swift app, which is out of scope.
2. **`Player` stays one concrete struct.** It is not the trait. Roughly half its
   methods never touch GStreamer and must not be duplicated per backend.
3. **Two adapters exist on day one:** the GStreamer adapter, and a `NullBackend`
   usable in tests. The second one is not speculative; see the `cfg(test)` stub
   below.
4. Dependency direction stays Swift to Rust. No Rust-to-Swift callbacks.

## What is above the seam (never touches GStreamer)

Verified by reading. These must be written once, in `Player`, not per backend.

| Cluster | Methods | Notes |
|---|---|---|
| `granite_*` | 6 | plasma visualizer, `src/granite/` has zero gst references |
| fadeout ramp | 4 | `begin_fadeout`, `poll_fadeout`, `cancel_fadeout`, `is_fading_out`; pure timing over a `fade_factor` scalar |
| `stop_after_current` | 3 | a bool plus take-once semantics |
| ReplayGain *policy* | 3 | `set_rg_album_mode`, `set_rg_fallback_db`, `set_rg_db_gain` |
| test hooks | 2 | `set_state_for_test`, `set_position_for_test` |

The volume path is the clearest illustration. `apply_output_volume` is four
lines of domain arithmetic ending in exactly one line that touches GStreamer:

```rust
self.volume_elem.set_property(
    "volume",
    self.user_volume * self.user_preamp * self.fade_factor,
);
```

The composition rule belongs to `Player`. Only the final scalar write belongs to
the backend.

## What is below the seam (GStreamer today)

Current `Player` fields that are GStreamer types: `pipeline`, `decodebin`,
`audioconvert`, `spectrum_elem`, `eq`, `volume_elem`, `rg_volume`, `rg_limiter`.
Plus `cdda_device`, `holds_disc_guard`, `spectrum_data`, `waveform_data`.

Transport, EQ writes, volume writes, bus polling, spectrum and waveform capture.

`BusEvent` (`engine.rs:40`) and `PlayerState` (`engine.rs:57`) are **already
backend-neutral enums** and already cross the FFI to Swift. They are the existing
seam vocabulary. Reuse them; do not invent parallel types.

## Three findings that constrain the design

These came from a measurement spike
(`docs/superpowers/plans/2026-08-31-macos-audio-backend-spike.md`). They are not
speculation.

1. **`poll_bus()` is a pull; AVFoundation is a push.** GStreamer polls a bus with
   zero timeout. AVFoundation delivers via delegates and callbacks on its own
   queues. The trait method should stay pull-shaped so `Player` and the UI tick
   loop are unchanged, and the AVFoundation adapter buffers pushed events into a
   drained queue. Design this deliberately.
2. **AVFoundation's `mainMixerNode` applies a flat -3.01 dB (1/√2) equal-power
   pan law** on a mono path where GStreamer's null is exactly 0.00 dB.
   Uncompensated, macOS ships quieter. Decide where that compensation lives:
   inside the adapter, or as an explicit part of the trait contract.
3. **Tap buffer size is advisory.** AVFoundation returned 4410-frame buffers when
   asked for 1024. Any spectrum or waveform consumer assuming its requested size
   will break.

## The `cfg(test)` stub the seam should delete

`src/engine.rs:276-283`:

```rust
#[cfg(not(test))]
let eq: Option<gst::Element> = gst::ElementFactory::make("equalizer-10bands")
    .name("equalizer").build().ok();
#[cfg(test)]
let eq: Option<gst::Element> = None;
```

The EQ element is hardcoded to `None` in tests, so the EQ element path is
untestable by construction. A `NullBackend` should make this stub deletable.
Treat "does this design let that `#[cfg(test)]` line go away" as a design
criterion.

## Existing tests, and one that will fight you

`cargo test --lib` is 768 passing, 0 failing, 23 ignored (all `live_*` hardware).

- `engine::eq_volume_pin` (7 tests, just committed) pins EQ clamping into the
  shadow copy `eq_bands`, and the `user_volume * user_preamp * fade_factor`
  composition read off `volume_elem`. Mutation-verified against six mutations.
  **Your design must keep these assertions expressible.**
- `engine::rg_tests` (5 tests) assert GStreamer **element graph shape** through
  private fields: `p.pipeline.by_name("rgvol")`, `feeds(&rgv, &rgl)`,
  `p.volume_elem`. These cannot survive the seam as written. Say explicitly in
  your rationale where ReplayGain lands: which part is `Player` policy, which is
  adapter mechanism, and where these five tests move to.
- `engine::live_cdda_tests` (10 tests) cover fadeout, transport, disc guard.
  Mostly above the seam, but check.

## Deliverable

Per the runner prompt: caller usage first, then the type sketch, function
signatures, module map, and the rationale. Bodies are `unimplemented!()`.

Answer these explicitly in the rationale:

- How many methods on the trait, and why that set? A 48-method trait is a failed
  design; so is one so thin that `Player` must know it is talking to GStreamer.
- Does `Player` hold `Box<dyn AudioBackend>`, a generic parameter, or a
  `cfg`-selected concrete type? Justify against the fact that the backend is
  chosen at compile time per platform, never swapped at runtime.
- Where does ReplayGain split?
- Where does spectrum and waveform capture live, given the buffer size is
  advisory and the two consumers want different shapes?
- Does the `#[cfg(test)]` EQ stub go away under your design?
