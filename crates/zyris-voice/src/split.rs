//! Deltas into fragments a synthesiser can be handed.
//!
//! # The spec's rule is inverted, and so was the correction
//!
//! The spec says perceived latency is time to first audio and nothing else, so the first
//! fragment may break at a comma. The step-8 plan overturned that, on the grounds that the
//! vector estimator has a fixed per-fragment cost and cutting finer therefore buys nothing.
//!
//! **Both were argued from one fragment length, and the second is wrong.** Measured here on
//! 2026-09-15, best of three at each length, models on disk, `M1`, eight flow steps:
//!
//! | characters | synthesis | audio | speech in it |
//! |---|---|---|---|
//! | 4 | 2.42 s | 1.29 s | 0.35 s |
//! | 12 | 2.16 s | 1.37 s | 0.59 s |
//! | 24 | 2.82 s | 2.00 s | 1.30 s |
//! | 40 | 2.94 s | 2.95 s | 1.99 s |
//! | 60 | 5.62 s | 4.27 s | 3.19 s |
//! | 90 | 6.69 s | 6.08 s | 5.02 s |
//! | 133 | 9.92 s | 8.42 s | 7.24 s |
//!
//! There **is** a per-fragment floor — about a second on this machine under load, 0.35–0.4 s on
//! an idle one — but it is not the whole cost: synthesis also runs at a **real-time factor of
//! 1.2 to 1.9**. It is slower than speech. So the queue never gets ahead, the speaker starves
//! after every fragment whatever the split, and "pipeline N+1 while N plays" — the plan's
//! argument for long fragments — **does not happen on this hardware at all**.
//!
//! What that leaves is a much simpler accounting. Speech ends at roughly *total synthesis +
//! the last fragment's audio*, so splitting trades work against how much audio is left in the
//! last fragment, and those two nearly cancel. The same 133 characters, cut three ways:
//!
//! | cut | fragments | first sound | total work | audio | speech ends |
//! |---|---|---|---|---|---|
//! | whole | 1 | **8.88 s** | 8.88 s | 8.42 s | ~17.3 s |
//! | two sentences | 2 | 6.98 s | 12.46 s | 9.12 s | ~17.1 s |
//! | six clauses | 6 | **2.40 s** | 16.37 s | 11.05 s | ~18.2 s |
//!
//! **The end barely moves and the beginning collapses.** Cutting the opening finer is very
//! nearly free in the only place a person notices, which is the spec's conclusion reached by
//! the opposite argument: not because a fragment is cheap, but because the machine is behind
//! either way and the wait is all paid at the front.
//!
//! What the fine cut *does* cost is audible, and it is not latency. Six fragments carry 11.05 s
//! of audio holding 6.03 s of speech: five seconds of silence, **about 0.9 s at every seam**,
//! because the model puts 0.3–0.6 s of lead-in and 0.35–0.6 s of tail on every fragment it
//! produces — see [`MODEL_LEAD_IN`]. A seam is therefore a full sentence-ending pause whether
//! or not a sentence ended there. Splitting mid-clause does not sound like a fast voice; it
//! sounds like a voice that keeps stopping.
//!
//! # So: sentences, except at the start
//!
//! - Cut at **terminal punctuation**, which is where a 0.9 s pause belongs. Never at a comma —
//!   [`MIN_CHARS`], [`MAX_CHARS`].
//! - **Except the first fragment**, which may end at a clause boundary once [`FIRST_MIN_CHARS`]
//!   characters have arrived. One unnaturally long pause early is worth six seconds off the
//!   wait for the first sound. Only the first: the rest of the answer is being spoken by then
//!   and nobody is waiting.
//! - **A minimum length**, because a fragment costs its floor whatever it says: "Yes." is
//!   2.42 s of work for 0.35 s of speech. Short sentences join the next one.
//! - **A cap**, because an answer can run for three lines without a full stop and must still be
//!   spoken.
//! - **A flush**, because the last sentence of a turn arrives without the whitespace that would
//!   prove it had ended, and because a stream can stall — [`IDLE_FLUSH`].
//!
//! # Only filtered text gets here
//!
//! [`Splitter::push`] takes an [`Aloud`], which has no public constructor: the only way to make
//! one is [`crate::speak::Filter`]. A delta cannot reach the voice without being filtered
//! first, and that is a property of the types rather than of anybody's memory.

use std::time::Duration;

use crate::speak::Aloud;

/// The shortest ordinary fragment, in characters.
///
/// A fragment pays the floor whatever is in it — 2.42 s of synthesis for "Yes." — and carries
/// a 0.9 s seam. What is left over as silence falls steeply and then stops falling: **57% of
/// what comes back at 12 characters, 35% at 24, 32% at 40, 14% at 133**. Almost all of the
/// improvement is between 12 and 24 characters, so the minimum sits at the top of that and a
/// sentence shorter than it joins the one after it.
///
/// It is deliberately close to [`FIRST_MIN_CHARS`]: the two differ by five characters, and the
/// distinction between an ordinary fragment and the first one is carried almost entirely by
/// *what counts as a boundary* rather than by how long it has to be.
pub const MIN_CHARS: usize = 25;

/// The shortest **first** fragment, in characters, and the only one allowed to end at a clause.
///
/// Twenty rather than nothing because the measurements say the saving stops there: 12
/// characters cost 2.16 s and 40 cost 2.94 s, so cutting below twenty saves under a second of
/// waiting and spends a whole extra fragment — a second of work and 0.9 s of seam — to do it.
pub const FIRST_MIN_CHARS: usize = 20;

/// The longest a fragment may grow before it is cut wherever it can be.
///
/// Answers with no terminal punctuation for three lines are ordinary — a list, a table, a line
/// of prose that runs on — and without a cap nothing is ever spoken. 120 characters is about
/// nine seconds of synthesis here, which is the longest anyone should wait for one seam to
/// fill; past 133 characters the efficiency has stopped improving anyway.
pub const MAX_CHARS: usize = 120;

/// How long a stream may say nothing before what is held is spoken anyway.
///
/// The turn's end is the ordinary flush and it arrives as a protocol frame, not as a timer.
/// This is for the other case: a stream that stalls mid-sentence. Three quarters of a second is
/// past any token-rate jitter and still less than one fragment's synthesis, so a flush it
/// causes is never the thing keeping the speaker waiting.
pub const IDLE_FLUSH: Duration = Duration::from_millis(750);

/// Silence the model puts in front of the first word of every fragment.
///
/// The smallest measured over the table in this module's documentation (the largest was 567 ms).
/// It is not a property of the text: "Yes." gets 379 ms of it.
pub const MODEL_LEAD_IN: Duration = Duration::from_millis(325);

/// Silence the model puts after the last word of every fragment. Smallest measured; the largest
/// was 619 ms.
pub const MODEL_TAIL: Duration = Duration::from_millis(352);

/// What a person hears as one sentence ending rather than as a voice that has stopped.
pub const SENTENCE_PAUSE: Duration = Duration::from_millis(350);

/// Silence to put **between** two fragments, on top of what they already carry.
///
/// **Zero, and it is derived rather than chosen.** The spec asks for 80–150 ms and the archived
/// reference implementation puts 0.3 s between its chunks; both are additions, and both were
/// written without [`MODEL_LEAD_IN`] and [`MODEL_TAIL`] in front of them. The model already
/// delivers **at least 677 ms** between the last word of one fragment and the first of the
/// next, and up to 1.19 s — twice to eight times the spec's figure, and already past what
/// [`SENTENCE_PAUSE`] says a pause should be. Adding to it makes an audible gap worse.
///
/// Written as a subtraction so that the argument survives the thing that would change it: if
/// the lead-in and tail are ever trimmed off a fragment before it is queued — which the
/// measurements say is worth doing, and which belongs with whoever hands samples to
/// `playback`, not here — these constants move and the gap reappears by itself.
pub const GAP: Duration = SENTENCE_PAUSE.saturating_sub(MODEL_TAIL.saturating_add(MODEL_LEAD_IN));

/// One piece of speech, ready to be synthesised.
///
/// A type rather than a `String` for [`Aloud`]'s reason one layer down: what comes out of here
/// has been through the filter *and* has been cut where a pause belongs, and neither is
/// recoverable by looking at the characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fragment(String);

impl Fragment {
    /// What to say.
    pub fn text(&self) -> &str {
        &self.0
    }
}

/// Deltas in, fragments out. **One per turn**: it remembers whether anything has been spoken
/// yet, which is what [`FIRST_MIN_CHARS`] is about.
#[derive(Debug, Default)]
pub struct Splitter {
    held: String,
    spoken: bool,
}

impl Splitter {
    /// A splitter at the start of a turn.
    pub fn new() -> Splitter {
        Splitter::default()
    }

    /// Add what a delta left to say, and take whatever that completed.
    ///
    /// Usually empty: most deltas are a few characters and a fragment is a sentence.
    pub fn push(&mut self, aloud: &Aloud) -> Vec<Fragment> {
        self.held.push_str(aloud.text());
        let mut out = Vec::new();
        while let Some(end) = self.cut() {
            let rest = self.held.split_off(end);
            let done = std::mem::replace(&mut self.held, rest.trim_start().to_string());
            let done = done.trim();
            if !done.is_empty() {
                out.push(Fragment(done.to_string()));
                self.spoken = true;
            }
        }
        out
    }

    /// Speak what is held, complete or not.
    ///
    /// The end of a turn, and the answer to [`IDLE_FLUSH`]. `None` when there is nothing held —
    /// which is the ordinary case at the end of an answer that ended in a full stop and a
    /// space.
    pub fn flush(&mut self) -> Option<Fragment> {
        let held = std::mem::take(&mut self.held);
        let held = held.trim();
        if held.is_empty() {
            return None;
        }
        self.spoken = true;
        Some(Fragment(held.to_string()))
    }

    /// What has arrived and is not yet a fragment.
    pub fn held(&self) -> &str {
        &self.held
    }

    /// Whether anything has been spoken in this turn yet. While this is false the next fragment
    /// is the first one, and [`FIRST_MIN_CHARS`] applies instead of [`MIN_CHARS`].
    pub fn started(&self) -> bool {
        self.spoken
    }

    /// Where to cut what is held, or `None` to wait for more.
    fn cut(&self) -> Option<usize> {
        let chars: Vec<(usize, char)> = self.held.char_indices().collect();
        let min = if self.spoken { MIN_CHARS } else { FIRST_MIN_CHARS };
        // A clause is a boundary only for the first fragment — and, below, for a fragment that
        // has hit the cap and has to be cut somewhere.
        let clause = !self.spoken;

        // The earliest boundary that leaves a fragment worth its floor.
        for p in min.saturating_sub(1)..chars.len().min(MAX_CHARS) {
            if let Some(end) = boundary(&chars, p, clause) {
                return Some(end);
            }
        }

        if chars.len() <= MAX_CHARS {
            return None;
        }
        // Past the cap. Any boundary, then any space, then the cap itself — in that order,
        // because each is a worse place to stop than the one before it.
        for p in (min.saturating_sub(1)..MAX_CHARS).rev() {
            if let Some(end) = boundary(&chars, p, true) {
                return Some(end);
            }
        }
        for p in (min..MAX_CHARS).rev() {
            if chars[p].1.is_whitespace() {
                return Some(chars[p].0);
            }
        }
        Some(chars[MAX_CHARS].0)
    }
}

/// Whether the character at `p` ends a fragment, and where the fragment ends if it does.
///
/// **A stop is not a boundary until something follows it**, which is what keeps `3.5` and a
/// version number in one piece: the digit after the point says it was not a sentence. At the
/// very end of what has arrived the answer is therefore "not yet" — [`Splitter::flush`] is what
/// resolves that, because more may be coming and only the caller knows.
///
/// Closing quotes and brackets come *after* the stop, so `said "no." Then` ends after the
/// quote and not between the two characters.
///
/// Known and left: an abbreviation — `e.g.`, `Mr.` — is a boundary. The cost is one pause in
/// the wrong place, against a table of abbreviations per language that would be wrong in a
/// different place.
fn boundary(chars: &[(usize, char)], p: usize, clause: bool) -> Option<usize> {
    let c = chars[p].1;
    let terminal = matches!(c, '.' | '!' | '?' | '\u{2026}' | '\u{3002}' | '\u{FF01}' | '\u{FF1F}');
    let clausal = clause && matches!(c, ',' | ';' | ':' | '\u{2014}' | '\u{2013}');
    if !terminal && !clausal {
        return None;
    }
    let mut q = p + 1;
    while q < chars.len()
        && matches!(
            chars[q].1,
            '.' | '!'
                | '?'
                | '"'
                | '\''
                | '\u{2019}'
                | '\u{201D}'
                | ')'
                | ']'
                | '}'
                | '\u{00BB}'
                | '\u{203A}'
        )
    {
        q += 1;
    }
    match chars.get(q) {
        Some((i, c)) if c.is_whitespace() => Some(*i),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::speak::{Filter, Kind};

    /// Everything here goes through the real filter, because that is the only way to make an
    /// [`Aloud`] — which is the point of [`Aloud`].
    fn feed(splitter: &mut Splitter, text: &str) -> Vec<String> {
        let mut filter = Filter::new();
        let aloud = filter.read(Kind::Assistant, text);
        let mut out: Vec<String> =
            splitter.push(&aloud.aloud).iter().map(|f| f.text().to_string()).collect();
        let rest = filter.finish();
        out.extend(splitter.push(&rest).iter().map(|f| f.text().to_string()));
        out
    }

    fn all(text: &str) -> Vec<String> {
        let mut splitter = Splitter::new();
        let mut out = feed(&mut splitter, text);
        out.extend(splitter.flush().iter().map(|f| f.text().to_string()));
        out
    }

    /// **The rule the whole module is about.** The first fragment may end at a clause; nothing
    /// after it may.
    ///
    /// The discriminator is the *second* comma: there is one at character 46 of the remainder,
    /// past [`MIN_CHARS`], and it must not produce a fragment. A splitter that allowed clauses
    /// everywhere passes a test that only looks at the first cut.
    #[test]
    fn the_first_fragment_may_end_at_a_clause_and_no_other_may() {
        let said = all(
            "The file is on disk now, and the checksum matches what the server said, \
             so nothing else has to be fetched.",
        );
        assert_eq!(
            said,
            vec![
                "The file is on disk now,",
                "and the checksum matches what the server said, so nothing else has to be \
                 fetched.",
            ],
            "the comma after `said` is past MIN_CHARS and is still not a boundary"
        );
    }

    /// Sentences are where the cuts are, once the first fragment has gone out — and a sentence
    /// under [`MIN_CHARS`] is not one of them.
    ///
    /// `and everything passed.` is 22 characters, so it joins the sentence after it rather than
    /// spending a whole floor on 1.3 s of speech. That merge is the discriminator: a splitter
    /// that cut at every stop produces four fragments here, all of them green under a test that
    /// only asserted the first cut.
    #[test]
    fn a_sentence_is_a_fragment_and_a_short_one_is_not() {
        let said = all(
            "The build finished cleanly, and everything passed. The tests took four minutes \
             to run. Nothing else is waiting.",
        );
        assert_eq!(
            said,
            vec![
                "The build finished cleanly,",
                "and everything passed. The tests took four minutes to run.",
                "Nothing else is waiting.",
            ]
        );
    }

    /// A sentence shorter than [`MIN_CHARS`] joins the next one rather than paying the floor.
    ///
    /// "Yes." is 2.42 s of synthesis for 0.35 s of speech and a 0.9 s seam after it.
    #[test]
    fn a_short_sentence_joins_the_one_after_it() {
        let mut splitter = Splitter::new();
        // Past the first fragment, so MIN_CHARS is what applies.
        splitter.spoken = true;
        let said = feed(&mut splitter, "Yes. Done. It worked. ");
        assert!(said.is_empty(), "none of those three is worth a fragment alone: {said:?}");
        let rest = feed(&mut splitter, "The whole thing is finished now. ");
        assert_eq!(rest, vec!["Yes. Done. It worked. The whole thing is finished now."]);
    }

    /// A full stop inside a number is not the end of a sentence.
    ///
    /// The rule that decides it is "a stop is not a boundary until whitespace follows", which
    /// is also what makes the last sentence of a turn wait for [`Splitter::flush`].
    #[test]
    fn a_decimal_point_is_not_a_sentence() {
        let mut splitter = Splitter::new();
        let said = feed(&mut splitter, "The whole run took 3.5 seconds of processor time and ");
        assert!(said.is_empty(), "nothing ended: {said:?}");
        assert!(splitter.held().contains("3.5"), "and the number is intact");
    }

    /// A stop at the very end of what has arrived is not a boundary yet: `3.` could be `3.5`.
    /// It is [`Splitter::flush`] that resolves it, because only the caller knows the turn ended.
    #[test]
    fn a_stop_at_the_end_of_what_arrived_waits_rather_than_guessing() {
        let mut splitter = Splitter::new();
        splitter.spoken = true;
        let said = feed(&mut splitter, "The build finished and everything passed.");
        assert!(said.is_empty(), "{said:?}");
        assert_eq!(
            splitter.flush().map(|f| f.text().to_string()),
            Some("The build finished and everything passed.".to_string())
        );
        assert_eq!(splitter.flush(), None, "and there is nothing left behind it");
    }

    /// The closing quote belongs to the sentence it closes.
    #[test]
    fn a_quote_after_a_stop_stays_with_its_sentence() {
        let mut splitter = Splitter::new();
        splitter.spoken = true;
        let said = feed(
            &mut splitter,
            "The server said \"nothing to do here.\" Then it closed the connection. ",
        );
        assert_eq!(
            said,
            vec![
                "The server said \"nothing to do here.\"",
                "Then it closed the connection.",
            ],
            "the cut is after the quote, not between the stop and it"
        );
    }

    /// Text with no terminal punctuation at all is still spoken, cut at the cap.
    #[test]
    fn a_run_on_is_cut_at_the_cap() {
        let mut splitter = Splitter::new();
        splitter.spoken = true;
        let long: String = std::iter::repeat_n("chalk ", 40).collect();
        let said = feed(&mut splitter, &long);
        assert!(!said.is_empty(), "nothing would ever be spoken");
        for fragment in &said {
            assert!(
                fragment.chars().count() <= MAX_CHARS,
                "{} characters is past the cap: {fragment}",
                fragment.chars().count()
            );
            assert!(!fragment.ends_with("chal"), "it was cut at a word: {fragment}");
        }
    }

    /// Whitespace alone is never a fragment: a fragment with nothing in it is 2.4 s of work to
    /// produce silence, and `tts` refuses it anyway.
    #[test]
    fn whitespace_is_not_a_fragment() {
        let mut splitter = Splitter::new();
        assert!(feed(&mut splitter, "   \n  \n ").is_empty());
        assert_eq!(splitter.flush(), None);
    }

    /// **The gap, and the reason it is nothing.**
    ///
    /// Asserting it is zero would pass for a constant somebody typed. What is asserted is the
    /// *derivation*: the model already leaves more than [`SENTENCE_PAUSE`] at every seam, so
    /// there is nothing to add — and both the spec's 80–150 ms and the reference
    /// implementation's 0.3 s would have made an audible gap worse.
    #[test]
    fn nothing_is_added_between_fragments_because_the_model_already_leaves_too_much() {
        let seam = MODEL_TAIL + MODEL_LEAD_IN;
        assert!(
            seam > SENTENCE_PAUSE,
            "measured {seam:?} of silence at a seam against a {SENTENCE_PAUSE:?} pause"
        );
        assert_eq!(GAP, Duration::ZERO);
        assert!(
            seam > Duration::from_millis(150) && seam > Duration::from_millis(300),
            "both the spec's figure and the reference's are additions to {seam:?}"
        );
    }

    /// The first fragment is not cut below [`FIRST_MIN_CHARS`], because the saving stops there.
    #[test]
    fn the_first_fragment_is_not_cut_shorter_than_the_floor_is_worth() {
        let mut splitter = Splitter::new();
        let said = feed(&mut splitter, "Yes, ok, fine, ");
        assert!(said.is_empty(), "three clauses inside twenty characters: {said:?}");
        let more = feed(&mut splitter, "the whole thing is finished, ");
        assert_eq!(more, vec!["Yes, ok, fine, the whole thing is finished,"]);
    }

    /// A fragment arrives one delta at a time and the boundary can land anywhere in one.
    #[test]
    fn a_sentence_spread_over_many_deltas_is_one_fragment() {
        let mut splitter = Splitter::new();
        splitter.spoken = true;
        let mut out = Vec::new();
        for delta in ["The build ", "finished and ", "everything ", "passed. ", "Next."] {
            out.extend(feed(&mut splitter, delta));
        }
        assert_eq!(out, vec!["The build finished and everything passed."]);
        assert_eq!(splitter.flush().map(|f| f.text().to_string()), Some("Next.".to_string()));
    }

    /// The filter and the splitter together: a fence, an aside and a URL, in one answer.
    #[test]
    fn the_two_halves_meet() {
        let said = all(
            "The file is at https://example.com/a/b now. Run this:\n\
             ```sh\nrm -rf /tmp/x\n```\nThen check it (it takes a moment) and tell me.",
        );
        let joined = said.join(" | ");
        assert!(!joined.contains("rm -rf"), "the fence's contents are not spoken: {joined}");
        assert!(!joined.contains("example.com"), "nor is the URL: {joined}");
        assert!(!joined.contains("takes a moment"), "nor is the aside: {joined}");
        assert!(joined.contains("code"), "the fence is one word: {joined}");
        assert!(joined.contains("link"), "and so is the URL: {joined}");
    }
}
