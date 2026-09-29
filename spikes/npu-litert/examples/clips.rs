//! Korean and English sentences spoken by Supertonic, at 16 kHz: the same clips on every phone.
use zyris_voice::capture::Conversion;
use zyris_voice::tts::{self, DEFAULT_VOICE, Tts, VoiceState};

const CLIPS: [(&str, &str); 4] = [
    ("ko1", "내일 아침 서울 날씨가 어떨지 알려줘."),
    ("en1", "What is the weather going to be like tomorrow morning?"),
    ("ko2", "오늘 회의 일정 정리해서 알려줄래?"),
    ("ko-long", "깃허브에 올라온 이슈 목록을 확인하고, 급한 것부터 세 개만 골라서 요약해 줘. 그리고 각 이슈 담당자에게 보낼 메시지 초안도 같이 써 줘."),
];

fn write_wav(path: &std::path::Path, samples: &[f32]) {
    let data: Vec<u8> = samples.iter().flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()).collect();
    let mut out = Vec::with_capacity(44 + data.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&16_000u32.to_le_bytes());
    out.extend_from_slice(&32_000u32.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    std::fs::write(path, out).expect("write wav");
}

fn main() {
    let VoiceState::Ready { dir } = tts::state() else { panic!("no Supertonic models; set {}", tts::MODELS_ENV) };
    let mut voice = Tts::load(&dir, DEFAULT_VOICE).expect("the voice loads");
    let out = std::path::Path::new("out/clips");
    std::fs::create_dir_all(out).expect("out/clips");
    let mut index = serde_json::Map::new();
    for (name, sentence) in CLIPS {
        let spoken = voice.say(sentence).expect("spoken");
        let mut conversion = Conversion::new(tts::SAMPLE_RATE, 1).expect("converts");
        let mut audio = conversion.feed(&spoken.samples).to_vec();
        audio.extend_from_slice(conversion.feed(&vec![0.0; tts::SAMPLE_RATE as usize / 2]));
        write_wav(&out.join(format!("{name}.wav")), &audio);
        index.insert(format!("{name}.wav"), sentence.into());
    }
    std::fs::copy("../../crates/zyris-voice/tests/audio/jfk.wav", out.join("jfk.wav")).expect("jfk");
    index.insert("jfk.wav".into(), "And so my fellow Americans, ask not what your country can do for you, ask what you can do for your country.".into());
    std::fs::write(out.join("clips.json"), serde_json::to_string_pretty(&index).unwrap()).expect("clips.json");
}
