# Where the ReplayGain coefficients came from

> **Not legal advice.** A record of what was done and why, so the reasoning can
> be checked rather than taken on trust.

## The problem, found on review

The first implementation of `src/replaygain/rg1.rs` was built by fetching
`gain_analysis.c` from mp3gain and extracting its coefficient tables with a
script. That file is:

```
Copyright (C) 2001-2009 David Robinson and Glen Sawyer
... GNU Lesser General Public License ... version 2.1 ... or later
...
concept and filter values by David Robinson
```

**LGPL-2.1-or-later, with the header explicitly claiming the filter values.**
All 299 coefficient literals in the generated file were byte-identical to it,
and the filter also carried `1e-10` — a denormal-avoidance constant that is an
implementation optimisation with no basis in the specification, and therefore
copied expression rather than a copied fact.

Licence *compatibility* was never the issue: LGPL-2.1-or-later can be taken as
LGPL-3.0, which combines fine into an AGPL-3.0 work. The issue was
**provenance**. Sparkamp ships to the App Store on the strength of one person
holding all the copyright, and code derived from Robinson and Sawyer's work
would mean that is no longer true — which is the foundation the whole
distribution arrangement rests on.

## What changed

The published ReplayGain 1.0 specification tabulates the coefficients itself,
at
<https://wiki.hydrogenaudio.org/index.php?title=ReplayGain_1.0_specification>,
tables 1 and 2. `coefficients.rs` is now generated from **that**, and the
`1e-10` is gone.

Three details make this a real re-sourcing rather than a relabelling:

1. **The specification publishes only 44.1 kHz and 48 kHz.** It says other
   rates "must be transformed to maintain the same filter response". The
   reference implementation carries ten more tables, and those transformed
   values are that project's work rather than the standard's — so they are no
   longer here. Other rates are refused, not approximated.
2. **The sign convention differs.** The specification tabulates `a(n)`
   positive; the reference stores them pre-negated for its unrolled filter.
   Generating from the specification means flipping the sign at generation,
   which the previous version did not have to do.
3. **The values were cross-checked.** Spec-derived and reference tables agree
   exactly for 44.1 kHz. They would: there is one correct set of numbers.

That last point is also the substance of the argument. Every conforming
ReplayGain 1.0 implementation — foobar2000, Winamp, mp3gain, GStreamer, ffmpeg
— uses these identical values, because using different ones produces something
that is not ReplayGain. Where an idea admits only one expression, the two
merge and the expression is not separately protected. These numbers *are* the
format.

## What remains shared, and why that is expected

`coefficients.rs` still has 52 of 53 literals in common with `gain_analysis.c`,
and always will: they are the same standard. What matters is that they are now
taken from the document that defines the standard rather than from someone's
implementation of it.

In `rg1.rs` the shared literals are `0.95` (the percentile), `0.050` (the
window, in seconds), `64.82` (the calibration constant) and arithmetic like
`0.5` and `1.0`. The first three are published in the specification and are
facts of the format.

## What was reimplemented rather than copied

The filter loops, the histogram, the percentile walk and the album
accumulation were written from the specification's description. They do not
share structure with the reference: it uses unrolled macros over pointer
arithmetic with `order` samples of context either side of each buffer, and
this keeps explicit history buffers and iterates. Algorithms are not
copyrightable in any case, but the two do not read alike.

## Residual risk, stated plainly

A determined reading could still argue that a table of 42 numbers has thin
compilation copyright and that the specification's own tables are themselves
Robinson's. The counter is merger, and it is strong — but it is an argument,
not a certainty, and it should be put to a lawyer alongside the CLA rather than
settled here.

If that answer comes back unfavourable, the fallback is EBU R128 (ITU-R
BS.1770), which is ReplayGain 2.0's basis and has permissively licensed
implementations. It would give different numbers from `rganalysis` by design,
which is a larger behavioural change than this was.
