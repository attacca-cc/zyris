//! Whether what was just said is the phrase somebody enrolled.
//!
//! Dynamic time warping over [`crate::mfcc`] features, against the five takes
//! [`crate::wake`] keeps. **No model, no training set, no download** — which is the constraint,
//! not a preference: the only thing this has to work from is five recordings a person made.
//!
//! # What this is and is not
//!
//! It is the classic template-matching keyword spotter. It handles the thing a straight
//! comparison cannot — that one phrase said twice is stretched differently — by finding the
//! cheapest monotonic alignment between two feature sequences and reporting what that
//! alignment cost per step.
//!
//! It is **not** a trained keyword model, and the difference shows up as false accepts on
//! phrases that merely sound similar. A neural spotter learns what distinguishes the phrase
//! from everything else in a language; this one knows only what the phrase itself looks like.
//! The published figure this project cited when deferring the work — 5.4 % misses at 0.1 false
//! accepts an hour — belongs to the trained kind and is not a prediction about this.
//!
//! # The threshold is calibrated, not chosen
//!
//! A distance means nothing on its own: it depends on the phrase, the speaker, the microphone
//! and the room. What *is* meaningful is how the distance to a new recording compares with the
//! distances the five takes have to **each other** — the same person saying the same phrase on
//! the same equipment is the best available picture of what "a match" costs.
//!
//! So enrolment measures that spread and the threshold is derived from it. See [`Spread`].
//! Where the takes disagree wildly with one another the threshold comes out loose and matching
//! will be unreliable, and that is a thing a person can be told before they rely on it.

use crate::mfcc::{DIMENSIONS, Features};

/// How far past the takes' own worst disagreement a recording may be and still be the phrase.
///
/// **This is the one number here that is a judgement rather than a measurement**, and it is a
/// multiplier rather than a distance so that it travels between phrases and speakers. 1.25
/// gives a quarter more room than the enrolment takes needed among themselves.
///
/// Which way to err is decided by what each error costs. A false accept starts a turn nobody
/// asked for: the microphone opens, whatever was said next goes to an agent, and the agent may
/// act on it — the same asymmetry [`crate::vad`] argues its speech floor from, and the reason
/// that floor errs high too. A false reject costs saying the phrase again, which a person can
/// see happening and fix themselves.
pub const ROOM: f32 = 1.25;

/// Never accept a distance above this, whatever the takes' spread says.
///
/// A person who recorded five takes in five different rooms, or who coughed in one, produces a
/// spread so wide that `ROOM` times it would accept nearly anything. The cap turns that into
/// "this never matches" rather than "this matches everything", which is the failure a person
/// can diagnose rather than one that starts turns all day.
///
/// **18 because that is under the closest a different phrase came, measured.** Five takes of
/// one phrase by one speaker, against four windows of `tests/audio/jfk.wav`:
///
/// | | distance |
/// |---|---|
/// | the takes to each other | 10.24 – 13.04 |
/// | a held-out take to the other four | 10.24 – 11.53 |
/// | the threshold those spreads give | 14.91 – 16.30 |
/// | **a different phrase, closest window** | **18.52** |
///
/// So this enrolment's own threshold sits below the cap and the cap never fires for it, which
/// is what a cap should do. **What that measurement does not cover is the dangerous negative**:
/// the same speaker in the same room saying something else. One recording of one other phrase
/// by another voice is the weakest possible negative set, and the honest reading is that this
/// separates a phrase from *unrelated* speech and has not been shown to separate it from
/// nearby speech.
pub const CEILING: f32 = 18.0;

/// What one take is, in the form matching works on.
#[derive(Debug, Clone)]
pub struct Template {
    rows: Vec<[f32; DIMENSIONS]>,
}

impl Template {
    pub fn of(features: &Features, samples: &[f32]) -> Template {
        Template { rows: features.of(samples) }
    }

    /// Whether there is anything to compare. Audio shorter than one frame has no rows.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn frames(&self) -> usize {
        self.rows.len()
    }
}

/// The cheapest alignment cost per step between two recordings.
///
/// **Per step, not in total**, or every comparison would prefer the shortest candidate — a
/// quarter-second of silence would beat the phrase itself. Dividing by the path length is what
/// makes distances from candidates of different lengths comparable at all.
///
/// The band is the usual Sakoe–Chiba one: an alignment may drift from the diagonal by at most
/// a fraction of the longer sequence. It is a speed-up and also a rule — an unbanded warp will
/// happily align a two-second phrase to one syllable of a ten-second sentence and report a
/// small cost for it.
pub fn distance(one: &Template, other: &Template) -> Option<f32> {
    let (a, b) = (&one.rows, &other.rows);
    if a.is_empty() || b.is_empty() {
        return None;
    }
    // A candidate wildly longer or shorter than the template is not the phrase, and saying so
    // here costs nothing. Without it the band below has to be wide enough to be useless.
    let ratio = a.len() as f32 / b.len() as f32;
    if !(0.5..=2.0).contains(&ratio) {
        return None;
    }
    let band = (a.len().max(b.len()) as f32 * 0.25).ceil() as usize;

    let wide = b.len() + 1;
    let mut previous = vec![f32::INFINITY; wide];
    let mut current = vec![f32::INFINITY; wide];
    // The path length alongside the cost, so the answer can be per step. Carried rather than
    // derived: the cheapest path and the shortest path are not always the same one.
    let mut previous_steps = vec![0usize; wide];
    let mut current_steps = vec![0usize; wide];
    previous[0] = 0.0;

    for i in 1..=a.len() {
        current.fill(f32::INFINITY);
        current_steps.fill(0);
        // Only the cells inside the band. `j` is 1-based like `i`.
        let centre = i * b.len() / a.len();
        let from = centre.saturating_sub(band).max(1);
        let to = (centre + band).min(b.len());
        for j in from..=to {
            let step = square_distance(&a[i - 1], &b[j - 1]);
            // Diagonal, up, left. Ties go to the diagonal, which is the alignment that warps
            // least.
            let (best, steps) = [
                (previous[j - 1], previous_steps[j - 1]),
                (previous[j], previous_steps[j]),
                (current[j - 1], current_steps[j - 1]),
            ]
            .into_iter()
            .fold((f32::INFINITY, 0usize), |(bc, bs), (cost, count)| {
                if cost < bc { (cost, count) } else { (bc, bs) }
            });
            if best.is_finite() {
                current[j] = best + step;
                current_steps[j] = steps + 1;
            }
        }
        std::mem::swap(&mut previous, &mut current);
        std::mem::swap(&mut previous_steps, &mut current_steps);
    }

    let total = previous[b.len()];
    let steps = previous_steps[b.len()];
    if !total.is_finite() || steps == 0 {
        // The band never reached the far corner, which a ratio inside the bound makes
        // unreachable — kept because an unreachable `None` is a refusal and an unreachable
        // `Some(0.0)` would be a perfect match.
        return None;
    }
    Some(total / steps as f32)
}

/// Euclidean distance between two frames. Not squared: squaring makes one badly-matched frame
/// dominate a whole alignment, which for speech means one consonant deciding the answer.
fn square_distance(a: &[f32; DIMENSIONS], b: &[f32; DIMENSIONS]) -> f32 {
    let mut sum = 0.0;
    for at in 0..DIMENSIONS {
        let difference = a[at] - b[at];
        sum += difference * difference;
    }
    sum.sqrt()
}

/// How much the enrolment takes disagree with one another, and the threshold that follows.
///
/// **This is what makes the threshold mean something.** A raw distance is uninterpretable — it
/// depends on the phrase, the voice, the microphone and the room. The takes give a picture of
/// what the same person saying the same phrase on the same equipment costs, and a candidate is
/// judged against that rather than against a number somebody typed.
#[derive(Debug, Clone, PartialEq)]
pub struct Spread {
    /// The largest distance between any two takes.
    pub worst: f32,
    /// The median of every pair. Reported because it is what says whether `worst` is the
    /// ordinary case or one bad take.
    pub middle: f32,
    /// How many pairs were comparable. Fewer than the takes allow means some take had no
    /// features or was wildly the wrong length.
    pub pairs: usize,
}

impl Spread {
    /// The distances among every pair of takes.
    pub fn of(templates: &[Template]) -> Spread {
        let mut distances = Vec::new();
        for (at, one) in templates.iter().enumerate() {
            for other in &templates[at + 1..] {
                if let Some(apart) = distance(one, other) {
                    distances.push(apart);
                }
            }
        }
        if distances.is_empty() {
            return Spread { worst: 0.0, middle: 0.0, pairs: 0 };
        }
        distances.sort_by(|a, b| a.partial_cmp(b).expect("distances are finite"));
        Spread {
            worst: *distances.last().expect("not empty"),
            middle: distances[distances.len() / 2],
            pairs: distances.len(),
        }
    }

    /// The distance a candidate has to beat.
    ///
    /// `None` when nothing could be compared — with no picture of what a match costs there is
    /// no honest threshold, and refusing to match is the answer that does not start turns
    /// nobody asked for.
    pub fn threshold(&self) -> Option<f32> {
        if self.pairs == 0 {
            return None;
        }
        Some((self.worst * ROOM).min(CEILING))
    }
}

/// The takes, ready to be compared against.
pub struct Phrase {
    templates: Vec<Template>,
    threshold: Option<f32>,
    spread: Spread,
}

/// What a comparison decided, and the number behind it.
#[derive(Debug, Clone, PartialEq)]
pub enum Match {
    /// The phrase, at this distance against this threshold.
    Yes { distance: f32, threshold: f32 },
    /// Compared and refused.
    No { distance: f32, threshold: f32 },
    /// Nothing to compare: no usable takes, or a candidate too short or too far off in length.
    /// **Not a poor match** — a screen and a log show them differently, and this one is the
    /// state a machine sits in forever rather than an answer about one utterance.
    Cannot { reason: &'static str },
}

impl Phrase {
    /// Build from the stored takes' audio.
    pub fn from_takes(features: &Features, takes: &[Vec<f32>]) -> Phrase {
        let templates: Vec<Template> = takes
            .iter()
            .map(|samples| Template::of(features, samples))
            .filter(|template| !template.is_empty())
            .collect();
        let spread = Spread::of(&templates);
        let threshold = spread.threshold();
        Phrase { templates, threshold, spread }
    }

    pub fn spread(&self) -> &Spread {
        &self.spread
    }

    pub fn threshold(&self) -> Option<f32> {
        self.threshold
    }

    /// Whether this recording is the phrase.
    ///
    /// **The best of the takes decides, not the average.** A person says a phrase several ways
    /// and the takes capture several of them; requiring a candidate to be close to all five
    /// would reject every utterance that happens to resemble one of them well and the others
    /// only roughly, which is the ordinary case.
    pub fn matches(&self, features: &Features, samples: &[f32]) -> Match {
        let Some(threshold) = self.threshold else {
            return Match::Cannot { reason: "no two enrolment takes could be compared" };
        };
        let candidate = Template::of(features, samples);
        if candidate.is_empty() {
            return Match::Cannot { reason: "the recording is shorter than one frame" };
        }
        let closest = self
            .templates
            .iter()
            .filter_map(|template| distance(&candidate, template))
            .min_by(|a, b| a.partial_cmp(b).expect("distances are finite"));
        match closest {
            None => Match::Cannot { reason: "nothing said is close to the length of the phrase" },
            Some(distance) if distance <= threshold => Match::Yes { distance, threshold },
            Some(distance) => Match::No { distance, threshold },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tone, which gives the front end something with real spectral structure rather than
    /// noise whose features are the same everywhere.
    fn tone(hz: f32, seconds: f32) -> Vec<f32> {
        let n = (crate::capture::SAMPLE_RATE as f32 * seconds) as usize;
        (0..n)
            .map(|at| {
                let t = at as f32 / crate::capture::SAMPLE_RATE as f32;
                0.4 * (2.0 * std::f32::consts::PI * hz * t).sin()
            })
            .collect()
    }

    /// Two tones one after the other, so a warp has something to align rather than a signal
    /// that is identical everywhere and therefore alignable anywhere at no cost.
    fn phrase(first: f32, second: f32, seconds: f32) -> Vec<f32> {
        let mut samples = tone(first, seconds / 2.0);
        samples.extend(tone(second, seconds / 2.0));
        samples
    }

    fn features() -> Features {
        Features::new()
    }

    #[test]
    fn a_recording_is_no_distance_from_itself() {
        let f = features();
        let one = Template::of(&f, &phrase(300.0, 900.0, 1.0));
        assert_eq!(distance(&one, &one), Some(0.0));
    }

    /// **The cost is per step and that is what makes two candidates comparable.** A total would
    /// grow with the length of the alignment, so a short recording would beat a long one at
    /// being any phrase — a quarter-second of silence would be the best match for everything.
    #[test]
    fn a_longer_recording_of_the_same_thing_is_not_further_away() {
        let f = features();
        let short = Template::of(&f, &phrase(300.0, 900.0, 1.0));
        let long = Template::of(&f, &phrase(300.0, 900.0, 1.8));

        let apart = distance(&short, &long).expect("comparable");
        let itself = distance(&short, &short).expect("comparable");
        assert!(
            apart < 2.0,
            "the same phrase said more slowly came out {apart} away, which is not a warp"
        );
        assert!(itself <= apart);
    }

    /// Two different phrases are further apart than one phrase said two ways. Everything else
    /// here rests on that being true of the front end at all.
    #[test]
    fn two_different_phrases_are_further_apart_than_one_said_twice() {
        let f = features();
        let one = Template::of(&f, &phrase(300.0, 900.0, 1.2));
        let again = Template::of(&f, &phrase(300.0, 900.0, 1.5));
        let other = Template::of(&f, &phrase(1500.0, 400.0, 1.3));

        let same = distance(&one, &again).expect("comparable");
        let different = distance(&one, &other).expect("comparable");
        assert!(
            same < different,
            "one phrase said twice ({same}) was not closer than two phrases ({different})"
        );
    }

    /// **A candidate wildly the wrong length is refused rather than scored.** Without this the
    /// warp will align a two-second phrase against one syllable of a ten-second sentence and
    /// report a small cost for it, which is a false accept with a confident number on it.
    #[test]
    fn a_recording_nothing_like_the_length_of_the_phrase_is_not_scored() {
        let f = features();
        let short = Template::of(&f, &phrase(300.0, 900.0, 1.0));
        let long = Template::of(&f, &phrase(300.0, 900.0, 4.0));

        assert_eq!(distance(&short, &long), None);
    }

    #[test]
    fn nothing_to_compare_is_not_a_poor_match() {
        let f = features();
        let real = Template::of(&f, &phrase(300.0, 900.0, 1.0));
        // Shorter than one frame.
        let nothing = Template::of(&f, &tone(300.0, 0.01));

        assert!(nothing.is_empty());
        assert_eq!(distance(&real, &nothing), None);
    }

    /// The threshold is the takes' own worst disagreement plus room, and the cap is what stops
    /// a wild enrolment accepting everything.
    #[test]
    fn the_threshold_comes_from_the_takes_and_is_capped() {
        let tight = Spread { worst: 4.0, middle: 3.0, pairs: 10 };
        assert_eq!(tight.threshold(), Some(4.0 * ROOM));

        let wild = Spread { worst: 100.0, middle: 90.0, pairs: 10 };
        assert_eq!(wild.threshold(), Some(CEILING));
    }

    /// **No picture of what a match costs means no match.** A threshold invented here would be
    /// a number about somebody else's voice, and the cost of being wrong is a turn nobody
    /// asked for going to an agent that can act on it.
    #[test]
    fn nothing_comparable_refuses_rather_than_guessing() {
        let nothing = Spread { worst: 0.0, middle: 0.0, pairs: 0 };
        assert_eq!(nothing.threshold(), None);

        let f = features();
        let empty = Phrase::from_takes(&f, &[]);
        assert!(matches!(empty.matches(&f, &phrase(300.0, 900.0, 1.0)), Match::Cannot { .. }));
    }

    /// The enrolled phrase is accepted and a different one is refused, end to end through the
    /// type a caller actually holds.
    #[test]
    fn the_phrase_is_accepted_and_another_is_not() {
        let f = features();
        let takes: Vec<Vec<f32>> = [1.0, 1.1, 1.2, 0.95, 1.05]
            .iter()
            .map(|seconds| phrase(300.0, 900.0, *seconds))
            .collect();
        let enrolled = Phrase::from_takes(&f, &takes);

        assert!(
            matches!(enrolled.matches(&f, &phrase(300.0, 900.0, 1.15)), Match::Yes { .. }),
            "the phrase itself was refused: {:?}",
            enrolled.matches(&f, &phrase(300.0, 900.0, 1.15))
        );
        assert!(
            matches!(enrolled.matches(&f, &phrase(1500.0, 400.0, 1.1)), Match::No { .. }),
            "a different phrase was accepted: {:?}",
            enrolled.matches(&f, &phrase(1500.0, 400.0, 1.1))
        );
    }

    /// **The real recordings, when a machine has some.** Everything above is synthetic, and
    /// synthetic tones say nothing about whether this works on a voice: the separation that
    /// matters was measured on five real takes and is written into [`CEILING`]'s
    /// documentation. This runs that measurement again wherever the takes exist, which on a
    /// runner is nowhere — the same gating `stt` and `tts` use, and the same cost.
    #[test]
    fn a_held_out_take_is_accepted_by_the_others() {
        let Ok(dir) = std::env::var(WAKE_TAKES) else {
            eprintln!("skipped: {WAKE_TAKES} does not name a directory of takes");
            return;
        };
        let store = crate::wake::Store::at(dir);
        let Ok(takes) = store.takes() else {
            eprintln!("skipped: the takes could not be read");
            return;
        };
        if takes.len() < 2 {
            eprintln!("skipped: fewer than two takes");
            return;
        }
        let all: Vec<Vec<f32>> = takes.iter().map(|t| t.samples().to_vec()).collect();
        let f = features();

        for held in 0..all.len() {
            let rest: Vec<Vec<f32>> = all
                .iter()
                .enumerate()
                .filter(|(at, _)| *at != held)
                .map(|(_, take)| take.clone())
                .collect();
            let phrase = Phrase::from_takes(&f, &rest);
            let verdict = phrase.matches(&f, &all[held]);
            assert!(
                matches!(verdict, Match::Yes { .. }),
                "take {} was not recognised by the other {}: {verdict:?}",
                held + 1,
                all.len() - 1
            );
        }
    }
}

/// Names a directory of enrolment takes, for the one test that can only run where some exist.
///
/// `stt::MODEL_ENV` and `tts::MODELS_ENV`'s neighbour, and it costs the same thing: **CI never
/// compares a real voice**, so the separation this module rests on is measured on a machine
/// that has recordings and nowhere else.
pub const WAKE_TAKES: &str = "ZYRIS_WAKE_TAKES";
