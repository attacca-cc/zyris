mod litert;
mod soc;
use std::time::Instant;
use zyris_voice::session::Transcribe;

fn arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

fn wav(path: &std::path::Path) -> Vec<f32> {
    let bytes = std::fs::read(path).unwrap();
    bytes[44..].chunks_exact(2).map(|s| f32::from(i16::from_le_bytes([s[0], s[1]])) / 32768.0).collect()
}

fn main() {
    let dir = std::path::PathBuf::from(arg("--model-dir").expect("--model-dir"));
    let clips = std::path::PathBuf::from(arg("--clips").expect("--clips"));
    let npu = arg("--accelerator").as_deref() != Some("cpu");
    let soc = soc::this_phone();
    let expected: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(clips.join("clips.json")).unwrap()).unwrap();

    let (graphs, dispatch) = match (npu, soc.htp) {
        (true, Some(v)) => (dir.join("npu").join(&soc.model), format!("/data/local/tmp/zyris-npu/qairt/v{v}")),
        (true, None) => {
            println!("soc={} vendor={} has no compiled model; falling back to the CPU", soc.model, soc.vendor);
            (dir.clone(), String::from("/data/local/tmp/zyris-npu"))
        }
        (false, _) => (dir.clone(), String::from("/data/local/tmp/zyris-npu")),
    };
    let on_npu = npu && soc.htp.is_some();
    let env = litert::Env::new(&dispatch).unwrap_or_else(|f| fail(&f));
    let started = Instant::now();
    let encoder = litert::Graph::open(&env, graphs.join("encoder.tflite").to_str().unwrap(), on_npu).unwrap_or_else(|f| fail(&f));
    let decoder = litert::Graph::open(&env, graphs.join("decoder.tflite").to_str().unwrap(), on_npu).unwrap_or_else(|f| fail(&f));
    let load_ms = started.elapsed().as_millis();
    println!(
        "soc={} htp={:?} accelerator={} load_ms={load_ms} encoder_fully_on_npu={} decoder_fully_on_npu={}",
        soc.model, soc.htp, if on_npu { "npu" } else { "cpu" }, encoder.fully_accelerated, decoder.fully_accelerated
    );
    if on_npu && !(encoder.fully_accelerated && decoder.fully_accelerated) {
        println!("warning: part of the graph runs on the CPU; the numbers below are mixed");
    }

    let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap();
    let specials = zyris_voice::onnx_stt::Specials::from_generation_config(&read("generation_config.json")).unwrap();
    let tokenizer = zyris_voice::bpe::Tokenizer::from_json(&read("tokenizer.json")).unwrap();
    let shapes: serde_json::Value = serde_json::from_str(&read("shapes.json")).unwrap();
    let runtime = litert::LiteRt::new(encoder, decoder, shapes["tokens"].as_u64().unwrap() as usize);
    let stt = zyris_voice::onnx_stt::OnnxStt::with(Box::new(runtime), specials, tokenizer, shapes["bins"].as_u64().unwrap() as usize);
    let engine = if on_npu { "litert-npu" } else { "litert-cpu" };
    let ggml = arg("--ggml").map(|p| zyris_voice::stt::Stt::load(std::path::Path::new(&p)).expect("whisper.cpp loads"));

    let mut names: Vec<_> = expected.keys().cloned().collect();
    names.sort();
    for name in names {
        let audio = wav(&clips.join(&name));
        let want = expected[&name].as_str().unwrap();
        let t = Instant::now();
        let first = stt.transcribe(&audio);
        let cold = t.elapsed().as_millis();
        use std::sync::atomic::Ordering::Relaxed;
        for c in [&litert::ENCODE_US, &litert::DECODE_US, &litert::DECODE_CALLS] {
            c.store(0, Relaxed);
        }
        let t = Instant::now();
        let text = stt.transcribe(&audio);
        let warm = t.elapsed().as_millis();
        let (enc, dec, calls) = (litert::ENCODE_US.load(Relaxed), litert::DECODE_US.load(Relaxed), litert::DECODE_CALLS.load(Relaxed));
        println!("split clip={name} encode_ms={} decode_ms={} decode_calls={calls} per_call_ms={:.1} rest_ms={}",
            enc / 1000, dec / 1000, dec as f64 / 1000.0 / calls.max(1) as f64, warm.saturating_sub(((enc + dec) / 1000) as u128));
        match (first, text) {
            (Ok(_), Ok(text)) => println!("clip={name} engine={engine} cold_ms={cold} warm_ms={warm} text={text:?} expected={want:?}"),
            (a, b) => println!("clip={name} engine={engine} error={:?}", a.err().or(b.err())),
        }
        if let Some(ggml) = &ggml {
            let _ = ggml.transcribe(&audio);
            let t = Instant::now();
            let text = ggml.transcribe(&audio);
            let ms = t.elapsed().as_millis();
            println!("clip={name} engine=whisper.cpp warm_ms={ms} text={:?} expected={want:?}", text.unwrap_or_default());
        }
    }
}

/// LiteRT refused: say so and exit non-zero, never carry on as if a CPU run were an NPU run.
fn fail(fault: &zyris_voice::stt::Fault) -> ! {
    eprintln!("error: {fault}");
    std::process::exit(2)
}
