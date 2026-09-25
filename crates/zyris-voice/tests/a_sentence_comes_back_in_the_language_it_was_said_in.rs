//! Korean in, Korean out: the voice reads a sentence aloud, the microphone's resampler takes it
//! down to 16 kHz, and whisper has to hand back the same sentence in the same script.
//!
//! What this guards is the pair of language decisions. Told `en`, whisper answers a Korean
//! sentence with **a different English sentence** rather than garbled Korean, and read with an
//! `<en>` tag the voice reads Hangul as noise — both measured, both silent in every other test.
//!
//! **Gated on both models**, like the other model tests: set `ZYRIS_TTS_MODELS` and
//! `ZYRIS_WHISPER_MODEL`, and build with `--release` — a debug whisper is slow enough to make
//! this take minutes (see `Stt::transcribe`).

#![cfg(feature = "voice")]

use zyris_voice::capture::Conversion;
use zyris_voice::stt::{self, Stt};
use zyris_voice::tts::{self, DEFAULT_VOICE, Tts, VoiceState};

/// What the voice says, run through the same conversion a 44.1 kHz microphone goes through.
fn said_to_a_microphone(tts: &mut Tts, text: &str) -> Vec<f32> {
    let spoken = tts.say(text).unwrap_or_else(|fault| panic!("{text:?}: {fault}"));
    let mut conversion = Conversion::new(tts::SAMPLE_RATE, 1).expect("44.1 kHz mono converts");
    let mut heard = conversion.feed(&spoken.samples).to_vec();
    // Half a second of room after the sentence, which also pushes the resampler's last chunk out.
    heard.extend_from_slice(conversion.feed(&vec![0.0; tts::SAMPLE_RATE as usize / 2]));
    heard
}

/// Only Hangul, Latin letters and digits, so punctuation and spacing whisper chooses differently
/// do not decide the comparison.
fn letters(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

#[test]
fn korean_and_english_each_come_back_as_themselves() {
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

    // Alternating, because a decoder state is reused between turns: a language must not stick.
    for sentence in [
        "내일 아침 서울 날씨가 어떨지 알려줘.",
        "What is the weather going to be like tomorrow morning?",
        "오늘 회의 일정 정리해서 알려줄래?",
    ] {
        let heard = stt.transcribe(&said_to_a_microphone(&mut tts, sentence)).expect("whisper answers");
        eprintln!("{sentence:?} -> {heard:?}");
        assert_eq!(
            tts::language_for(&heard),
            tts::language_for(sentence),
            "{sentence:?} came back in another script: {heard:?}"
        );
        assert_eq!(letters(&heard), letters(sentence), "{sentence:?} came back as {heard:?}");
    }
}
