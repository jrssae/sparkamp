# Decisions taken during synthesis

> Written during the architect fan-out that produced the `AudioBackend` seam,
> 31 August 2026, and moved into the repository on 2 September because it lived
> in a session scratchpad that was about to be discarded. It is the reasoning
> behind `2026-08-31-audio-backend-seam-design.md`, which is the design of
> record.
>
> Four candidate designs were commissioned in parallel and judged against each
> other. References to "opus", "sonnet", "haiku" and "fable" are those
> candidates, and "the judge" is the cross-model review that ranked them.

## D1. `rg_mid_play_change_defers_to_load`, GStreamer half

**Decision (Josef, 2026-08-31): assert against real pipeline state.** Not an
`#[ignore]`d live test.

### Why this was an open question

Today the test never plays anything. `set_state_for_test(PlayerState::Playing)`
writes `Player`'s own `state` field, and `set_replaygain` consults that field to
decide whether to defer. The load step uses `file:///nonexistent.mp3`. The whole
test is synthetic, and `engine.rs:1601` already admits in a comment that an empty
pipeline cannot reach Playing.

Under the seam the adapter reads its own real state, so the lie stops working.

### What makes the decision cheap

Two facts, both measured rather than assumed.

1. **No CI runs the test suite.** `gtk-check.yml` runs
   `cargo check --all-targets --locked` on ubuntu-latest. It compiles tests; it
   never executes them. The string "cargo test" appears in that workflow only
   inside a comment. So there is no headless-CI failure mode to design around.
2. **A real pipeline reaches PLAYING without audio hardware.** Verified with
   `gst-launch-1.0`:

   ```
   uridecodebin ! audioconvert ! volume ! equalizer-10bands ! fakesink sync=false
   ```

   reaches PLAYING on a generated silent WAV. The same chain with
   `autoaudiosink` also reaches PLAYING on a Mac with a device, but a container
   has none, and `scripts/flatpak-dev.sh:6` documents an Arch distrobox as the
   normal `cargo test` environment.

### Implementation constraint this places on the design

`GstBackend` must let a test choose the sink. `Player` hardcodes
`autoaudiosink` today at `engine.rs:238`. The test needs a genuinely real
pipeline that genuinely transitions to Playing, which `fakesink sync=false`
provides, so the sink becomes a construction parameter of the GStreamer adapter
rather than a hardcoded element.

The test then needs a decodable fixture. Generating a short silent WAV in the
test is enough; no committed binary fixture is required.

This keeps the assertion honest. The state is the pipeline's, not a field
someone wrote to.
