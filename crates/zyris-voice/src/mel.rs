//! Whisper's log-mel spectrogram, as Hugging Face's `WhisperFeatureExtractor` computes it.
//!
//! **The reference is Hugging Face's, not whisper.cpp's**, because the exported models this feeds
//! were exported from those weights and evaluated on those features. The numbers the tests pin
//! were produced by `transformers`' `WhisperFeatureExtractor(feature_size=80)` on 2026-09-29.

/// Thirty seconds at 16 kHz: every input is padded or cut to exactly this.
pub const SAMPLES: usize = 480_000;
/// Frames in thirty seconds, which is what the encoder takes and nothing else.
pub const FRAMES: usize = 3000;

use realfft::RealFftPlanner;

const N_FFT: usize = 400;
const HOP: usize = 160;
/// Frequency bins of one 400-point real FFT.
const FFT_BINS: usize = N_FFT / 2 + 1;

/// `bins * FRAMES` values, bin-major: `[b * FRAMES + t]`. The encoder's `[1, bins, 3000]`.
///
/// Padded with zeros or cut to [`SAMPLES`]; a centred STFT with reflect padding and a periodic
/// Hann window; power spectrum; Slaney mel filters; log10 with a 1e-10 floor; clamped to 8 below
/// the loudest value; then `(x + 4) / 4`. The STFT has 3001 frames, and the last is dropped, as
/// Hugging Face does.
pub fn log_mel(audio: &[f32], bins: usize) -> Vec<f32> {
    let mut padded = vec![0f32; SAMPLES];
    let kept = audio.len().min(SAMPLES);
    padded[..kept].copy_from_slice(&audio[..kept]);

    // `center=True`, reflect: 200 samples mirrored at each end, the edge sample not repeated.
    let half = N_FFT / 2;
    let mut x = Vec::with_capacity(SAMPLES + N_FFT);
    x.extend((1..=half).rev().map(|i| padded[i]));
    x.extend_from_slice(&padded);
    x.extend((SAMPLES - 1 - half..SAMPLES - 1).rev().map(|i| padded[i]));

    let window: Vec<f32> = (0..N_FFT)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / N_FFT as f32).cos())
        .collect();
    let fft = RealFftPlanner::<f32>::new().plan_fft_forward(N_FFT);
    let mut frame = fft.make_input_vec();
    let mut spectrum = fft.make_output_vec();
    let filters = mel_filters(bins);
    let mut power = vec![0f32; FFT_BINS];
    let mut mel = vec![0f32; bins * FRAMES];

    for t in 0..FRAMES {
        let start = t * HOP;
        for (i, sample) in frame.iter_mut().enumerate() {
            *sample = x[start + i] * window[i];
        }
        fft.process(&mut frame, &mut spectrum)
            .expect("buffers come from the plan");
        for (p, c) in power.iter_mut().zip(&spectrum) {
            *p = c.norm_sqr();
        }
        for b in 0..bins {
            let row = &filters[b * FFT_BINS..(b + 1) * FFT_BINS];
            let energy: f32 = row.iter().zip(&power).map(|(f, p)| f * p).sum();
            mel[b * FRAMES + t] = energy.max(1e-10).log10();
        }
    }

    let top = mel.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    for value in &mut mel {
        *value = (value.max(top - 8.0) + 4.0) / 4.0;
    }
    mel
}

/// Slaney's mel scale: linear below 1 kHz, logarithmic above.
fn hz_to_mel(hz: f64) -> f64 {
    if hz < 1000.0 {
        3.0 * hz / 200.0
    } else {
        15.0 + (hz / 1000.0).ln() * 27.0 / 6.4f64.ln()
    }
}

fn mel_to_hz(mel: f64) -> f64 {
    if mel < 15.0 {
        200.0 * mel / 3.0
    } else {
        1000.0 * ((mel - 15.0) * 6.4f64.ln() / 27.0).exp()
    }
}

/// `bins` triangular filters over 0-8 kHz, Slaney-normalised, row-major `[b * FFT_BINS + k]`.
fn mel_filters(bins: usize) -> Vec<f32> {
    let (low, high) = (hz_to_mel(0.0), hz_to_mel(8000.0));
    let edges: Vec<f64> = (0..bins + 2)
        .map(|i| mel_to_hz(low + (high - low) * i as f64 / (bins + 1) as f64))
        .collect();
    let mut filters = vec![0f32; bins * FFT_BINS];
    for b in 0..bins {
        let (left, centre, right) = (edges[b], edges[b + 1], edges[b + 2]);
        let norm = 2.0 / (right - left);
        for k in 0..FFT_BINS {
            let hz = 8000.0 * k as f64 / (FFT_BINS - 1) as f64;
            let rising = (hz - left) / (centre - left);
            let falling = (right - hz) / (right - centre);
            filters[b * FFT_BINS + k] = (rising.min(falling).max(0.0) * norm) as f32;
        }
    }
    filters
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(hz: f32, seconds: f32) -> Vec<f32> {
        (0..(16_000.0 * seconds) as usize)
            .map(|n| 0.5 * (2.0 * std::f32::consts::PI * hz * n as f32 / 16_000.0).sin())
            .collect()
    }

    #[test]
    fn silence_is_the_floor_everywhere() {
        let mel = log_mel(&vec![0.0; 16_000], 80);
        assert_eq!(mel.len(), 80 * FRAMES);
        assert!(
            mel.iter().all(|v| (v - -1.5).abs() < 1e-6),
            "silence is -1.5 throughout"
        );
    }

    #[test]
    fn no_audio_at_all_is_silence() {
        let mel = log_mel(&[], 80);
        assert_eq!(mel.len(), 80 * FRAMES);
        assert!(mel.iter().all(|v| (v - -1.5).abs() < 1e-6));
    }

    /// Hugging Face, same input: frame 10 peaks at bin 26 with 1.4396517, and everything under
    /// the 80 dB floor sits at -0.5603483.
    #[test]
    fn a_kilohertz_tone_lands_where_hugging_face_puts_it() {
        let mel = log_mel(&tone(1000.0, 1.0), 80);
        let (peak_bin, peak) = (0..80).map(|b| mel[b * FRAMES + 10]).enumerate().fold(
            (0, f32::MIN),
            |best, (b, v)| if v > best.1 { (b, v) } else { best },
        );
        assert_eq!(peak_bin, 26);
        assert!((peak - 1.439_651_7).abs() < 1e-3, "peak {peak}");
        assert!((mel[10] - -0.560_348_3).abs() < 1e-3, "bin 0 {}", mel[10]);
        assert!(
            (mel[2000] - -0.560_348_3).abs() < 1e-3,
            "after the tone ends {}",
            mel[2000]
        );
    }

    #[test]
    fn more_than_thirty_seconds_is_cut_to_thirty() {
        let long = tone(440.0, 31.0);
        assert_eq!(log_mel(&long, 80), log_mel(&long[..SAMPLES], 80));
    }

    #[test]
    fn large_v3_takes_128_bins() {
        assert_eq!(log_mel(&tone(440.0, 1.0), 128).len(), 128 * FRAMES);
    }
}
