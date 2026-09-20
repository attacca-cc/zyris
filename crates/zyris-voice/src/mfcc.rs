//! What a phrase sounds like, as numbers two recordings of it can be compared on.
//!
//! Mel-frequency cepstral coefficients: the front end almost every keyword spotter has used
//! since the 1980s, and the reason it is here rather than something newer is that it needs no
//! model file, no training data and no download. [`crate::wake`] stores five recordings of one
//! phrase and nothing else; whatever compares them has to work from those five alone.
//!
//! # Why not the samples themselves
//!
//! Two recordings of one phrase differ in loudness, in microphone, in room, and in exactly
//! which millisecond each syllable starts. Comparing waveforms measures all of that and almost
//! nothing about the words. The mel filterbank throws away phase and fine frequency detail, the
//! logarithm turns a change in loudness into an offset, and the cosine transform concentrates
//! what is left into the first few coefficients — after which subtracting the mean per
//! coefficient removes the offset, and with it most of what the room and the microphone
//! contributed. What survives is roughly the shape of the vocal tract over time, which is the
//! thing two recordings of one phrase have in common.
//!
//! Timing is **not** handled here: two people saying one phrase, or one person saying it twice,
//! stretch it differently. That is what the dynamic time warp in [`crate::wake`] is for.
//!
//! # What is measured and what is assumed
//!
//! Everything below is the standard recipe and each number is the usual one; none of them was
//! tuned against this project's own recordings, because tuning a front end on five takes of one
//! phrase is how a matcher comes to work for exactly those five recordings. What *is* calibrated
//! here is the decision threshold, and it is calibrated from the takes at enrolment time — see
//! [`crate::wake::Spread`].

use realfft::RealFftPlanner;

use crate::capture::SAMPLE_RATE;

/// 25 ms at 16 kHz. Long enough that the lowest voiced pitch fits inside it several times,
/// short enough that the vocal tract has not moved much within one.
pub const FRAME: usize = 400;

/// 10 ms. The usual hop, and the same 160 samples the processor already works in.
pub const HOP: usize = 160;

/// The FFT length: the power of two at or above [`FRAME`].
const FFT: usize = 512;

/// How many triangular filters the spectrum is collapsed onto.
const FILTERS: usize = 26;

/// How many cepstral coefficients are kept.
///
/// Thirteen is the usual figure and the zeroth is dropped below, so twelve travel. The zeroth
/// is overall energy, which is loudness — exactly what two recordings of one phrase are most
/// likely to differ in and least likely to differ in *meaningfully*.
const KEPT: usize = 13;

/// The lowest and highest frequency the filterbank covers.
///
/// 300 Hz is under the lowest voiced pitch and above most room rumble; 8 kHz is the Nyquist
/// limit of this rate, so nothing is discarded at the top.
const LOW_HZ: f32 = 300.0;
const HIGH_HZ: f32 = SAMPLE_RATE as f32 / 2.0;

/// Standard pre-emphasis. Lifts the quieter high frequencies where consonants live.
const PRE_EMPHASIS: f32 = 0.97;

/// A floor under the logarithm, so a digitally silent frame is a large negative number rather
/// than an infinity. An infinity would poison every later mean and make the whole take useless.
const FLOOR: f32 = 1e-10;

/// One frame's worth: [`KEPT`] minus the energy coefficient.
pub const DIMENSIONS: usize = KEPT - 1;

/// Hertz to mel, and back. O'Shaughnessy's formula, which is the one everybody means.
fn to_mel(hz: f32) -> f32 {
    2595.0 * (1.0 + hz / 700.0).log10()
}

fn from_mel(mel: f32) -> f32 {
    700.0 * (10f32.powf(mel / 2595.0) - 1.0)
}

/// The triangular filters, as (start, peak, end) bin indices.
fn filterbank() -> Vec<(usize, usize, usize)> {
    let low = to_mel(LOW_HZ);
    let high = to_mel(HIGH_HZ);
    // `FILTERS + 2` points, because each filter spans from its predecessor's centre to its
    // successor's: the first and last points are edges that no filter peaks at.
    let bin = |at: usize| {
        let mel = low + (high - low) * at as f32 / (FILTERS + 1) as f32;
        let hz = from_mel(mel);
        ((hz * FFT as f32 / SAMPLE_RATE as f32).round() as usize).min(FFT / 2)
    };
    (0..FILTERS).map(|filter| (bin(filter), bin(filter + 1), bin(filter + 2))).collect()
}

/// The transform, and the scratch it reuses.
///
/// Built once and used for every take and every candidate, because the FFT planner is the
/// expensive part and the windows and filterbank are the same for all of them.
pub struct Features {
    fft: std::sync::Arc<dyn realfft::RealToComplex<f32>>,
    window: Vec<f32>,
    bank: Vec<(usize, usize, usize)>,
    /// `cos((filter + 0.5) * coefficient * PI / FILTERS)`, laid out coefficient-major.
    dct: Vec<f32>,
}

impl Default for Features {
    fn default() -> Features {
        Features::new()
    }
}

impl Features {
    pub fn new() -> Features {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FFT);
        // Hamming rather than Hann: the usual choice here, and the difference between them is
        // far smaller than the difference between two recordings of one phrase.
        let window = (0..FRAME)
            .map(|at| {
                0.54 - 0.46 * (2.0 * std::f32::consts::PI * at as f32 / (FRAME - 1) as f32).cos()
            })
            .collect();
        let mut dct = Vec::with_capacity(KEPT * FILTERS);
        for coefficient in 0..KEPT {
            for filter in 0..FILTERS {
                dct.push(
                    ((filter as f32 + 0.5) * coefficient as f32 * std::f32::consts::PI
                        / FILTERS as f32)
                        .cos(),
                );
            }
        }
        Features { fft, window, bank: filterbank(), dct }
    }

    /// Everything a recording sounds like, one row per 10 ms.
    ///
    /// Empty for audio shorter than one frame, which the caller has to treat as "nothing to
    /// compare" rather than as a poor match — see [`crate::wake::Match`].
    pub fn of(&self, samples: &[f32]) -> Vec<[f32; DIMENSIONS]> {
        if samples.len() < FRAME {
            return Vec::new();
        }
        let mut rows = Vec::with_capacity((samples.len() - FRAME) / HOP + 1);
        let mut framed = vec![0.0f32; FFT];
        let mut spectrum = self.fft.make_output_vec();
        let mut energies = vec![0.0f32; FILTERS];

        let mut at = 0;
        while at + FRAME <= samples.len() {
            let frame = &samples[at..at + FRAME];
            // Pre-emphasis across the frame, with the sample before it where there is one, so
            // the filter does not restart at every hop.
            let before = if at == 0 { frame[0] } else { samples[at - 1] };
            framed[..FRAME].fill(0.0);
            for i in 0..FRAME {
                let previous = if i == 0 { before } else { frame[i - 1] };
                framed[i] = (frame[i] - PRE_EMPHASIS * previous) * self.window[i];
            }
            framed[FRAME..].fill(0.0);

            // `process` is allowed to scribble on its input, which is why `framed` is refilled
            // from scratch above rather than only having its tail cleared.
            self.fft.process(&mut framed, &mut spectrum).expect("the lengths are fixed");

            for (filter, &(start, peak, end)) in self.bank.iter().enumerate() {
                let mut sum = 0.0;
                for bin in start..=end.min(spectrum.len() - 1) {
                    // The triangle: up to the peak, down after it. A filter whose start and
                    // peak round to the same bin contributes nothing rather than dividing by
                    // zero — that happens at the bottom of the range, where the bins are
                    // closer together than the mel spacing.
                    let weight = if bin <= peak {
                        if peak == start { 0.0 } else { (bin - start) as f32 / (peak - start) as f32 }
                    } else if end == peak {
                        0.0
                    } else {
                        (end - bin) as f32 / (end - peak) as f32
                    };
                    sum += weight * spectrum[bin].norm_sqr();
                }
                energies[filter] = (sum + FLOOR).ln();
            }

            let mut row = [0.0f32; DIMENSIONS];
            // From 1, not 0: the zeroth coefficient is loudness. See `KEPT`.
            for coefficient in 1..KEPT {
                let base = coefficient * FILTERS;
                let mut sum = 0.0;
                for filter in 0..FILTERS {
                    sum += energies[filter] * self.dct[base + filter];
                }
                row[coefficient - 1] = sum;
            }
            rows.push(row);
            at += HOP;
        }

        mean_normalise(&mut rows);
        rows
    }
}

/// Subtract each coefficient's own mean over the whole recording.
///
/// **Cepstral mean normalisation, and it is not a nicety.** A microphone's frequency response
/// and a room's colouring are close to constant over a two-second phrase, and in the log domain
/// constant means *added*. Subtracting the mean removes them. Without it, the same phrase
/// recorded on a laptop microphone and on a headset compares as two different phrases — which
/// is precisely the case a wake word has to survive, because enrolment and use are minutes or
/// months apart.
fn mean_normalise(rows: &mut [[f32; DIMENSIONS]]) {
    if rows.is_empty() {
        return;
    }
    let mut mean = [0.0f32; DIMENSIONS];
    for row in rows.iter() {
        for (at, value) in row.iter().enumerate() {
            mean[at] += value;
        }
    }
    for value in mean.iter_mut() {
        *value /= rows.len() as f32;
    }
    for row in rows.iter_mut() {
        for (at, value) in row.iter_mut().enumerate() {
            *value -= mean[at];
        }
    }
}
