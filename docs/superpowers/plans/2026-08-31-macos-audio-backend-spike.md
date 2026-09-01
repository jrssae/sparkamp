# macOS Audio Backend Spike: can AVFoundation replace GStreamer?

> **For agentic workers:** this is a *spike*, not an implementation plan. Its
> only product is evidence and a decision. Prototype code is disposable and
> lives outside the repo. Do not merge prototype code; do not start the seam
> refactor from inside this document.

**Goal:** decide whether the Mac App Store build can run on AVFoundation
instead of GStreamer, and if so, in which language the adapter is written.

**Status: executed 2026-08-31. Verdict below.**

---

## Why this spike exists

The App Store work has one decision that gates every other task, and it cannot
be reasoned to a conclusion, only measured.

GStreamer is currently the deepest App Store blocker, and it is three blockers
wearing one coat:

| Blocker | Where |
|---|---|
| Plugins are `dlopen`'d from a path set by a shell-script launcher, and the Mac App Store rejects script main-executables | `packaging/macos/build-dmg.sh:323` |
| liborc JIT-compiles SIMD kernels into RWX pages, forcing `com.apple.security.cs.allow-unsigned-executable-memory` | `packaging/macos/entitlements.plist:8-11` |
| ~40 MB of bundled dylibs, each needing a signature, plus a per-plugin licence audit | `packaging/macos/build-dmg.sh:246-300` |

Replacing the backend removes all three at once. Bundling GStreamer properly
removes none of them, it merely makes them survivable.

The replacement has a failure mode that no amount of design work detects. The
interface can match perfectly while the audio changes character. That is what
this spike measured.

---

## Verdict

**Go to AVFoundation, and write the adapter in Rust via `objc2`.**

Both halves are supported by measurement. The one real gap is band 0, and it is
a GStreamer defect rather than an AVFoundation shortfall. Details below.

| Question | Result |
|---|---|
| Q1 EQ parity | Pass for 9 of 10 bands. Band 0 cannot match, and should not. |
| Q2 PCM taps | Pass, zero sample loss |
| Q3 Transport | Not run. Deprioritised once Q1/Q2/Q4 settled the decision. |
| Q4 Language | Rust via `objc2`. 78 lines, one `unsafe` block, working tap. |

---

## Findings

### Method

White noise, 12 s, 44.1 kHz mono float32. Both engines process the same file.
Magnitude response is Welch-averaged over a 32768-point FFT (1.35 Hz bins, which
the 29 Hz band needs; at 8192 the bottom two bands are unmeasurable), compared
against the dry signal, then sampled at sixth-octave probe points with a
proportional smoothing window so low and high frequencies get comparable
treatment.

### The reference: what `equalizer-10bands` actually is

`GstIirEqualizer10Bands`, a Direct Form 10-band IIR filter. The plugin is
`equalizer` from **gst-plugins-good, LGPL, not GPL**, so the EQ itself was never
a licence blocker.

Band centres are declared in `gst-inspect-1.0` and confirmed by measurement for
bands 1 through 8. Gain range is **-24 dB to +12 dB, asymmetric**, which is where
`CLAUDE.md`'s "Max +12 to avoid panic" comes from.

| Band | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 |
|---|---|---|---|---|---|---|---|---|---|---|
| Hz | 29 | 59 | 119 | 237 | 474 | 947 | 1889 | 3770 | 7523 | 15011 |

Bands sit one octave apart, which is why an `AVAudioUnitEQ` bandwidth of exactly
1.0 octave is the right match.

> **Correction, found while writing the adapter.** That sentence is right for
> the eight parametric bands and wrong for the two shelves. A plain `.lowShelf`
> or `.highShelf` has no bandwidth parameter, so writing one to bands 0 and 9 is
> undefined rather than merely redundant. Across 40 fresh engines the two shelf
> bands read back something other than what was written **41 times out of 80**,
> while the eight parametric bands were exact **320 out of 320**. It surfaced as
> an intermittent test failure, not a compile error. Write bandwidth only to the
> parametric bands.

Measured shapes: bands 1 through 8 are peaking filters at their declared
centres. Band 0 behaves as a low shelf and band 9 as a high shelf running to
Nyquist. Configuring `AVAudioUnitEQ` with `.lowShelf` at index 0 and
`.highShelf` at index 9 roughly halves the error versus all-parametric (band 9
RMS goes from 1.86 dB to 0.55 dB).

### Q1: EQ parity. Pass for 9 bands.

With bandwidth 1.0 octave and shelves on the edges:

| Test vector | Worst | RMS |
|---|---|---|
| flat | +0.00 dB | 0.00 dB |
| band 5 (947 Hz) +12 | +0.32 dB | **0.13 dB** |
| band 5 (947 Hz) -12 | -0.32 dB | **0.13 dB** |
| band 9 (15 kHz) +12 | -1.68 dB | 0.55 dB |
| band 0 (29 Hz) +12 | -4.89 dB | 1.58 dB |
| V shape | -4.84 dB | 1.71 dB |
| all +12 | +5.18 dB | 1.88 dB |

Every large error sits between 20 Hz and 90 Hz, which is band 0's territory.
Bands 1 through 8 track GStreamer to a tenth of a dB.

**A trap worth recording.** Before compensation every comparison was offset by a
flat -3.01 dB at every frequency. That is 1/√2, the equal-power pan law
`mainMixerNode` applies on a mono path. GStreamer's null case is exactly 0.00 dB.
This looks exactly like a broad filter error and is not one. **The real adapter
must compensate for it or macOS ships measurably quieter than Linux.**

### Band 0 is a GStreamer defect, not an AVFoundation shortfall

This is the finding that decides the question. With +12 dB requested at 29 Hz,
GStreamer produces:

| Hz | 20.0 | 25.2 | 31.7 | 35.6 | 40.0 | 44.9 | 50.4 | 63.5 | 80.0 |
|---|---|---|---|---|---|---|---|---|---|
| dB | +14.39 | +10.85 | +4.67 | +1.44 | **-1.13** | **-2.35** | **-2.77** | -2.22 | -1.44 |

Asking for a boost at 29 Hz produces a **2.8 dB cut at 50 Hz**. The undershoot
scales linearly with the requested gain (-0.86 dB at +3, -1.63 dB at +6, -2.35 dB
at +12, all measured at 45 Hz), so it is a genuine filter property, not
measurement noise. Band 1 at 59 Hz shows no undershoot at all and is a textbook
peaking filter.

This is what a Direct Form biquad does at a centre frequency 1520x below the
sample rate. No standard shelf or peak filter reproduces it, which is why no
`AVAudioUnitEQ` configuration got band 0 below about 1.4 dB RMS across a sweep of
filter types (`lowShelf`, `resonantLowShelf`, `parametric`) and corner
frequencies (20 to 120 Hz).

**Recommendation: do not chase parity on band 0.** Reproducing it means
reproducing a bug that no user asked for. Ship the correct low shelf and note
the change. If bit-exact parity is ever required, it needs a hand-written
biquad, not `AVAudioUnitEQ`.

### Q2: PCM taps. Pass.

`installTap(onBus:bufferSize:format:block:)` delivered **529,200 frames against
a 529,200-frame source. No loss.**

- 120 callbacks over the file, on a background thread, never the main thread.
- **`bufferSize` is advisory.** Requested 1024, received 4410 consistently. Any
  consumer assuming its requested size will break.
- Timestamps from `AVAudioTime.sampleTime` are monotonic and correct (0.000 to
  11.900 s for a 12 s file), so they can drive `position()` without a separate
  clock.

Both existing consumers are satisfiable: a raw sample ring for
`get_waveform_samples`, and a vDSP FFT for `get_spectrum_display_bands`. The
measurement harness for this spike used exactly that vDSP path, so it is proven,
not assumed. There is no AVFoundation equivalent of GStreamer's `spectrum`
element, so **we compute the FFT ourselves via Accelerate.**

### Q4: Rust via `objc2`. Recommended.

`objc2-avf-audio` 0.3.2 exposes everything needed, including the part that was
expected to be the blocker:

- `installTapOnBus_bufferSize_format_block` with a typed `AVAudioNodeTapBlock`.
  A Rust closure passed as an `RcBlock` type-checked as
  `dyn Fn(NonNull<AVAudioPCMBuffer>, NonNull<AVAudioTime>)` with no hand-rolled
  trampoline.
- All five EQ band setters: `setFilterType`, `setFrequency`, `setBandwidth`,
  `setGain`, `setBypass`.

A working probe that builds the full graph, configures ten bands, installs a
Rust tap, and plays a file came to **78 lines with a single `unsafe` block**, and
measured 66,150 frames in 1.5 s, exactly 44100/sec.

**One gap:** `renderOffline:toBuffer:error:` is not generated in 0.3.2, so
offline rendering from Rust needs `manualRenderingBlock` instead. This affects
test harnesses, not production playback, since real playback renders to the
output device.

Per the decision rule in this brief, the bindings cover the tap block without
hand-written glue, so Rust wins and the Swift-vtable inversion is unnecessary.
**The dependency direction stays Swift to Rust, unchanged.**

---

## What this changes

1. **The App Store blocker list loses three entries at once.** No `dlopen`'d
   plugins, no shell-script launcher, no
   `allow-unsigned-executable-memory`, because liborc leaves with GStreamer.
   The entitlement in `packaging/macos/entitlements.plist` can be deleted
   outright for the Mac build.
2. **The GPL-plugin audit shrinks to nothing** for the App Store build. Worth
   noting the EQ plugin was LGPL anyway.
3. **`AudioBackend` gains two requirements neither design pass would have
   predicted:** compensating the mixer's -3.01 dB pan law, and treating tap
   buffer size as advisory.
4. **A deliberate, documented behaviour change:** the 29 Hz band stops
   undershooting. This is an improvement, but it is a change, and it belongs in
   release notes.

## Next

The trait design is now unblocked and has real constraints to design against.
Sequencing from the plan holds: extract the seam with GStreamer as the only
adapter first, verify green on both platforms, then add the AVFoundation
adapter.

Out of scope here and still pending: ReplayGain chain equivalence
(`rgvolume`/`rglimiter`), and Q3 transport mapping, which should be folded into
the adapter work rather than re-spiked.
