//! A turn's deltas, all the way to samples, through the parts rather than around them.
//!
//! Each layer has its own tests and `session.rs` drives the whole machine against doubles. What
//! nothing exercised is the three real types in a row: a delta reaches [`speak::Filter`], what
//! survives it reaches [`split::Splitter`], and what that cuts reaches the synthesiser. A bug
//! that lives in the joins — a fragment that arrives with the filter's markup still in it, a
//! reasoning delta that is filtered and then re-added by the splitter's own buffering — is
//! invisible to every one of those suites and to this one's own doubles.
//!
//! **The synthesis half is gated**, because the models are 401 MB and `cargo test` must not
//! fetch them. Set `ZYRIS_TTS_MODELS` to a directory holding them and the last test runs; without
//! it, it says so and passes. **So CI reaches the text half of this file and not the audio half**
//! — the same hole `stt.rs` records for transcription, and for the same reason.

#![cfg(feature = "voice")]

use zyris_voice::speak::{Filter, Kind};
use zyris_voice::split::Splitter;

/// One turn's worth of deltas, as an agent writes them: a word at a time, split at no boundary
/// anyone chose, with a reasoning aside in the middle and a code fence at the end.
fn deltas() -> Vec<(Kind, &'static str)> {
    vec![
        (Kind::Assistant, "The file is "),
        (Kind::Reasoning, "I should check whether it exists first."),
        (Kind::Assistant, "already there. I moved it earlier"),
        (Kind::Assistant, " today. Run "),
        (Kind::Assistant, "```sh\nls -l /tmp\n```"),
        (Kind::Assistant, " to see it."),
    ]
}

/// Everything the voice would say for that turn, as fragments, through the real types.
fn spoken(deltas: &[(Kind, &'static str)]) -> Vec<String> {
    let mut filter = Filter::default();
    let mut splitter = Splitter::default();
    let mut out = Vec::new();

    for (kind, text) in deltas {
        let reading = filter.read(*kind, text);
        for fragment in splitter.push(&reading.aloud) {
            out.push(fragment.text().to_string());
        }
    }
    let last = filter.finish();
    for fragment in splitter.push(&last) {
        out.push(fragment.text().to_string());
    }
    if let Some(fragment) = splitter.flush() {
        out.push(fragment.text().to_string());
    }
    out
}

#[test]
fn a_reasoning_delta_never_reaches_a_fragment() {
    // Rule 1, asserted at the far end rather than where it is decided. The filter has its own
    // test for the decision; this one is about whether anything downstream puts it back.
    let said = spoken(&deltas()).join(" ");
    assert!(
        !said.contains("should check") && !said.to_lowercase().contains("exists first"),
        "a reasoning delta reached the voice: {said}"
    );
}

#[test]
fn a_code_fence_becomes_one_word_and_not_its_contents() {
    let said = spoken(&deltas()).join(" ");
    assert!(!said.contains("ls -l"), "the fence's contents reached the voice: {said}");
    assert!(said.contains("code"), "the fence did not become a word: {said}");
}

#[test]
fn what_the_agent_wrote_comes_out_in_order_and_whole() {
    let said = spoken(&deltas()).join(" ");
    for wanted in ["The file is", "already there", "to see it"] {
        assert!(said.contains(wanted), "{wanted:?} is missing from {said:?}");
    }
    let first = said.find("The file is").expect("the opening is there");
    let last = said.find("to see it").expect("the ending is there");
    assert!(first < last, "the fragments came out of order: {said}");
}

#[test]
fn a_turn_that_stops_mid_sentence_still_gives_up_what_it_has() {
    // An answer that ends without terminal punctuation is the ordinary case for a cancelled or
    // truncated turn, and nothing downstream would ever ask for it again.
    let said = spoken(&[(Kind::Assistant, "One moment while I look")]);
    assert!(!said.is_empty(), "a turn with no full stop produced no fragment at all");
    assert!(said.join(" ").contains("One moment"), "{said:?}");
}

#[test]
fn every_fragment_is_something_a_synthesiser_can_be_handed() {
    // The splitter's output is what goes to the model, so a fragment that is empty or is only
    // punctuation is a call that costs a fixed ~380 ms and produces nothing worth hearing.
    for fragment in spoken(&deltas()) {
        assert!(
            fragment.chars().any(|c| c.is_alphanumeric()),
            "a fragment with nothing speakable in it: {fragment:?}"
        );
    }
}

#[test]
fn the_fragments_of_a_turn_are_spoken_as_audio() {
    use zyris_voice::tts::{DEFAULT_VOICE, MODELS_ENV, Tts, VoiceState, state};
    let VoiceState::Ready { dir } = state() else {
        eprintln!("skipped: no Supertonic models; set {MODELS_ENV}");
        return;
    };
    let mut tts = Tts::load(&dir, DEFAULT_VOICE).expect("the models load");

    let fragments = spoken(&deltas());
    assert!(!fragments.is_empty(), "nothing to say");

    let mut total = 0usize;
    for fragment in &fragments {
        let said = tts.say(fragment).unwrap_or_else(|fault| panic!("{fragment:?}: {fault}"));
        assert!(!said.samples.is_empty(), "{fragment:?} produced no audio");
        assert!(
            said.samples.iter().all(|s| s.is_finite()),
            "{fragment:?} produced a sample that is not a number"
        );
        total += said.samples.len();
    }

    // Long enough to be speech rather than a click, at the model's own rate.
    assert!(total > 44_100 / 2, "the whole turn came to {total} samples");
}
