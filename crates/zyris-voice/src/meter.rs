//! How loud a stream is, a few dozen times a second.
//!
//! **For a picture, not for a decision.** The window draws the conversation's backdrop from
//! these numbers and nothing else reads them: the endpointer, the wake word and barge-in all
//! decide on their own evidence. So this is the plainest measure there is — root mean square
//! over a fixed window — and it is compiled without the `voice` feature, because it depends on
//! nothing and its tests should run everywhere.

/// How many levels a second each source publishes, at most. Twenty-five is smooth enough for a
/// drawing that is itself smoothed, and slow enough to be nothing on the channel.
pub const LEVELS_PER_SECOND: u32 = 25;

/// Which stream a [`Level`] was measured on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Source {
    /// What the microphone heard while a turn was open — never between turns.
    Microphone,
    /// What was written to the speaker.
    Speaker,
}

/// One measurement: the root mean square of the last window, as linear amplitude in `0..=1`.
///
/// Linear rather than decibels on purpose: how a number becomes a size on screen is the
/// window's business, and it is a one-line function there.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Level {
    pub source: Source,
    pub rms: f32,
}

/// Root mean square over consecutive windows of `window` samples, fed in pieces of any size.
#[derive(Debug, Clone)]
pub struct Meter {
    window: usize,
    sum: f64,
    count: usize,
}

impl Meter {
    /// A meter that reports once every `window` samples. A window of zero is taken as one.
    pub fn new(window: usize) -> Meter {
        Meter { window: window.max(1), sum: 0.0, count: 0 }
    }

    /// Add samples. Answers the level of the last window that completed during this call, if
    /// any did; the samples after it start the next one.
    ///
    /// **The last, not the first**, when one call completes several: a caller publishing levels
    /// wants what the stream sounds like now, and an older window is already out of date.
    pub fn push(&mut self, samples: &[f32]) -> Option<f32> {
        let mut completed = None;
        for &sample in samples {
            let sample = f64::from(sample);
            self.sum += sample * sample;
            self.count += 1;
            if self.count == self.window {
                completed = Some((self.sum / self.window as f64).sqrt() as f32);
                self.sum = 0.0;
                self.count = 0;
            }
        }
        completed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_scale_square_wave_is_one() {
        let mut meter = Meter::new(4);
        assert_eq!(meter.push(&[1.0, -1.0, 1.0, -1.0]), Some(1.0));
    }

    #[test]
    fn silence_is_zero() {
        let mut meter = Meter::new(8);
        assert_eq!(meter.push(&[0.0; 8]), Some(0.0));
    }

    #[test]
    fn a_window_spans_pushes_and_is_reported_only_once_complete() {
        let mut meter = Meter::new(640);
        assert_eq!(meter.push(&[0.5; 300]), None);
        let level = meter.push(&[0.5; 400]).expect("the window completed");
        assert!((level - 0.5).abs() < 1e-6, "{level}");
        // 60 samples carried over into the next window, which is not complete yet.
        assert_eq!(meter.push(&[0.5; 579]), None);
        assert!(meter.push(&[0.5; 1]).is_some());
    }

    #[test]
    fn when_one_push_completes_two_windows_the_last_is_reported() {
        let mut meter = Meter::new(2);
        let mut samples = vec![1.0, 1.0];
        samples.extend([0.0, 0.0]);
        assert_eq!(meter.push(&samples), Some(0.0));
    }

    #[test]
    fn a_zero_window_does_not_divide_by_zero() {
        let mut meter = Meter::new(0);
        assert_eq!(meter.push(&[0.25]), Some(0.25));
    }

    #[test]
    fn a_level_is_serialised_as_the_window_reads_it() {
        let level = Level { source: Source::Microphone, rms: 0.5 };
        assert_eq!(
            serde_json::to_string(&level).unwrap(),
            r#"{"source":"microphone","rms":0.5}"#
        );
        let level = Level { source: Source::Speaker, rms: 0.0 };
        assert_eq!(serde_json::to_string(&level).unwrap(), r#"{"source":"speaker","rms":0.0}"#);
    }
}
