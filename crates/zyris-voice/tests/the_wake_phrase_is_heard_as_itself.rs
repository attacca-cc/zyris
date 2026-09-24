//! The wake phrase, read the way the watch reads everything said in the room: whisper, told to
//! expect the phrase. The phrase has to come back as the phrase, and speech that merely sounds
//! near it has to keep its own words.
//!
//! Gated on both models, like the other model tests: set `ZYRIS_TTS_MODELS` and
//! `ZYRIS_WHISPER_MODEL`, and build with `--release`.

#![cfg(feature = "voice")]

use zyris_voice::capture::Conversion;
use zyris_voice::stt::{self, Stt};
use zyris_voice::tts::{self, DEFAULT_VOICE, Tts, VoiceState};
use zyris_voice::wake::{DEFAULT_PHRASE, Heard, Phrase};

fn said(tts: &mut Tts, text: &str) -> Vec<f32> {
    let spoken = tts.say(text).unwrap_or_else(|fault| panic!("{text:?}: {fault}"));
    let mut conversion = Conversion::new(tts::SAMPLE_RATE, 1).expect("44.1 kHz mono converts");
    let mut heard = conversion.feed(&spoken.samples).to_vec();
    heard.extend_from_slice(conversion.feed(&vec![0.0; tts::SAMPLE_RATE as usize / 10]));
    heard
}

#[test]
fn the_phrase_wakes_it_and_near_misses_do_not() {
    let VoiceState::Ready { dir } = tts::state() else {
        eprintln!("skipped: no Supertonic models; set {}", tts::MODELS_ENV);
        return;
    };
    let Some(model) = std::env::var_os(stt::MODEL_ENV).map(std::path::PathBuf::from) else {
        eprintln!("skipped: set {} to a ggml-base.bin to run this", stt::MODEL_ENV);
        return;
    };
    let mut tts = Tts::load(&dir, DEFAULT_VOICE).expect("the voice loads");
    let stt = Stt::load(&model).expect("whisper loads");
    let phrase = Phrase::new(DEFAULT_PHRASE).expect("letters");
    let hear = |audio: &[f32]| {
        let text = stt.transcribe_expecting(audio, Some(phrase.said())).expect("whisper answers");
        (phrase.in_(&text), text)
    };

    let (verdict, text) = hear(&said(&mut tts, "Hey Zyris."));
    assert_eq!(verdict, Heard::Phrase, "the phrase was heard as {text:?}");

    // The same, with a request after it, in Korean: the request keeps its own language.
    let (verdict, text) = hear(&said(&mut tts, "Hey Zyris, 오늘 날씨 알려줘."));
    match verdict {
        Heard::PhraseThen(rest) => assert_eq!(
            tts::language_for(&rest),
            "ko",
            "the request came back as {rest:?} in {text:?}"
        ),
        other => panic!("{other:?} for {text:?}"),
    }

    for other in ["Hey Siri.", "Hi there.", "하이 자비스.", "안녕하세요.", "오늘 날씨 어때?"] {
        let (verdict, text) = hear(&said(&mut tts, other));
        assert_eq!(verdict, Heard::Other, "{other:?} woke it, heard as {text:?}");
    }
}
