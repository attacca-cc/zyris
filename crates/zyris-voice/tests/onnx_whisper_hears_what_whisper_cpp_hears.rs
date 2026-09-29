//! Whisper run as an exported ONNX model hears what whisper.cpp hears: the same sentence from
//! `jfk.wav`, and Korean and English spoken by the voice.
//!
//! **Gated on `ZYRIS_ONNX_WHISPER`**, a directory holding `onnx-community/whisper-base` at
//! revision 1846881b6b3a3024392c1eea3ad983695bc23925: `config.json`, `generation_config.json`,
//! `tokenizer.json`, `onnx/encoder_model.onnx` and `onnx/decoder_model.onnx`. The spoken half
//! also needs `ZYRIS_TTS_MODELS`. Build with `--release`.

#![cfg(feature = "voice")]

use zyris_voice::capture::{Conversion, SAMPLE_RATE};
use zyris_voice::onnx_stt::{MODEL_ENV, OnnxStt};
use zyris_voice::session::Transcribe;
use zyris_voice::tts::{self, DEFAULT_VOICE, Tts, VoiceState};

fn model() -> Option<OnnxStt> {
    let dir = std::path::PathBuf::from(std::env::var_os(MODEL_ENV)?);
    Some(OnnxStt::load(&dir).unwrap_or_else(|fault| panic!("{}: {fault}", dir.display())))
}

fn letters(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// `tests/audio/jfk.wav`, 16 kHz mono 16-bit, with the reader the other model tests use.
fn jfk() -> Vec<f32> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/audio/jfk.wav");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let (mut rate, mut samples, mut at) = (0u32, Vec::new(), 12);
    while at + 8 <= bytes.len() {
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().expect("4 bytes")) as usize;
        let body = &bytes[at + 8..(at + 8 + size).min(bytes.len())];
        match &bytes[at..at + 4] {
            b"fmt " => rate = u32::from_le_bytes(body[4..8].try_into().expect("4 bytes")),
            b"data" => {
                samples = body
                    .chunks_exact(2)
                    .map(|s| {
                        f32::from(i16::from_le_bytes(s.try_into().expect("2 bytes"))) / 32768.0
                    })
                    .collect()
            }
            _ => {}
        }
        at += 8 + size + (size & 1);
    }
    assert_eq!(rate, SAMPLE_RATE);
    samples
}

#[test]
fn the_president_is_heard_word_for_word() {
    let Some(stt) = model() else {
        eprintln!("skipped: set {MODEL_ENV} to an exported whisper-base directory");
        return;
    };
    let started = std::time::Instant::now();
    let heard = stt.transcribe(&jfk()).expect("transcribes");
    eprintln!("jfk.wav -> {heard:?} in {:?}", started.elapsed());
    assert_eq!(
        letters(&heard),
        letters(
            "And so my fellow Americans, ask not what your country can do for you, \
             ask what you can do for your country."
        )
    );
}

#[test]
fn korean_and_english_each_come_back_as_themselves() {
    let Some(stt) = model() else {
        eprintln!("skipped: set {MODEL_ENV} to an exported whisper-base directory");
        return;
    };
    let VoiceState::Ready { dir } = tts::state() else {
        eprintln!("skipped: no Supertonic models; set {}", tts::MODELS_ENV);
        return;
    };
    let mut voice = Tts::load(&dir, DEFAULT_VOICE).expect("the voice loads");
    for sentence in [
        "내일 아침 서울 날씨가 어떨지 알려줘.",
        "What is the weather going to be like tomorrow morning?",
        "오늘 회의 일정 정리해서 알려줄래?",
    ] {
        let spoken = voice
            .say(sentence)
            .unwrap_or_else(|fault| panic!("{sentence:?}: {fault}"));
        let mut conversion = Conversion::new(tts::SAMPLE_RATE, 1).expect("44.1 kHz mono converts");
        let mut audio = conversion.feed(&spoken.samples).to_vec();
        audio.extend_from_slice(conversion.feed(&vec![0.0; tts::SAMPLE_RATE as usize / 2]));
        let heard = stt.transcribe(&audio).expect("transcribes");
        eprintln!("{sentence:?} -> {heard:?}");
        assert_eq!(
            tts::language_for(&heard),
            tts::language_for(sentence),
            "{heard:?}"
        );
        assert_eq!(
            letters(&heard),
            letters(sentence),
            "{sentence:?} came back as {heard:?}"
        );
    }
}
