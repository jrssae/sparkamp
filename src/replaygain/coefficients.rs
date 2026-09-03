//! ReplayGain 1.0 equal-loudness filter coefficients.
//!
//! # Provenance
//!
//! Taken from the **published ReplayGain 1.0 specification** (David Robinson,
//! 2001), tables 1 and 2, at
//! <https://wiki.hydrogenaudio.org/index.php?title=ReplayGain_1.0_specification>.
//! They are the numbers that define the standard: every conforming
//! implementation uses these and no others, which is what makes them a
//! statement of the format rather than anybody's code.
//!
//! Deliberately **not** taken from the reference implementation
//! `gain_analysis.c`, which is LGPL-2.1-or-later and whose header claims the
//! filter values. An earlier draft of this file did take them from there, and
//! was regenerated from the specification instead — see
//! `docs/superpowers/plans/2026-09-02-replaygain-provenance.md`.
//!
//! The specification publishes **44.1 kHz and 48 kHz only**, and says other
//! rates "must be transformed to maintain the same filter response". Those
//! transformed tables exist in the reference implementation, are that
//! project's work rather than the standard's, and are not reproduced here.
//! Anything else is resampled or refused.
//!
//! # Layout
//!
//! Interleaved: `[b0, -a1, b1, -a2, b2, ...]`. Even indices are feed-forward
//! coefficients applied to the input; odd indices are feedback coefficients
//! applied to previous output and subtracted. The specification tabulates
//! `a(n)` positive, so the sign is flipped here once, at generation, rather
//! than in the filter loop:
//!
//!   y[n] = k0*x[n] - k1*y[n-1] + k2*x[n-1] - k3*y[n-2] + k4*x[n-2] - ...

//! Only macOS routes to these, and the module is compiled everywhere anyway,
//! so off macOS the whole file is dead by construction. Silence that case
//! alone: a genuinely unused item still warns where the code actually runs.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

/// Sample rates the specification publishes coefficients for.
pub const RATES: [u32; 2] = [48000, 44100];

/// 10th-order Yule-Walker filter approximating the inverse equal-loudness
/// contour. Specification table 2.
pub const YULE: [[f64; 21]; 2] = [
    // 48000 Hz — specification table 2a
    [0.03857599435200, -3.84664617118067, -0.02160367184185, 7.81501653005538, -0.00123395316851, -11.34170355132042, -0.00009291677959, 13.05504219327545, -0.01655260341619, -12.28759895145294, 0.02161526843274, 9.48293806319790, -0.02074045215285, -5.87257861775999, 0.00594298065125, 2.75465861874613, 0.00306428023191, -0.86984376593551, 0.00012025322027, 0.13919314567432, 0.00288463683916],
    // 44100 Hz — specification table 2b
    [0.05418656406430, -3.47845948550071, -0.02911007808948, 6.36317777566148, -0.00848709379851, -8.54751527471874, -0.00851165645469, 9.47693607801280, -0.00834990904936, -8.81498681370155, 0.02245293253339, 6.85401540936998, -0.02596338512915, -4.39470996079559, 0.01624864962975, 2.19611684890774, -0.00240879051584, -0.75104302451432, 0.00674613682247, 0.13149317958808, -0.00187763777362],
];

/// 2nd-order Butterworth high-pass at 150 Hz. Specification table 1.
pub const BUTTER: [[f64; 5]; 2] = [
    // 48000 Hz — specification table 1a
    [0.98621192462708, -1.97223372919527, -1.97242384925416, 0.97261396931306, 0.98621192462708],
    // 44100 Hz — specification table 1b
    [0.98500175787242, -1.96977855582618, -1.97000351574484, 0.97022847566350, 0.98500175787242],
];
