"""Greedy decode with the three .tflite graphs, language detected, against transformers.generate."""
import hashlib, json, pathlib

import numpy as np
import soundfile
from ai_edge_litert.interpreter import Interpreter
from transformers import WhisperFeatureExtractor, WhisperTokenizer

import whisper_kv as kv

OUT = pathlib.Path("out/whisper-small")

def runner(name):
    r = Interpreter(model_path=str(OUT / f"{name}.tflite")).get_signature_runner()
    names = sorted(r.get_input_details().keys(), key=lambda n: int(n.split("_")[-1]))
    return lambda *xs: list(r(**dict(zip(names, xs))).values())

def transcribe(wav, gen, shapes, suffix=""):
    audio, rate = soundfile.read(wav, dtype="float32"); assert rate == 16000
    feats = WhisperFeatureExtractor(feature_size=shapes["bins"])(audio, sampling_rate=16000, return_tensors="np").input_features.astype(np.float32)
    if suffix:  # the short set reads the head of the same padded window, as the app does
        feats = np.ascontiguousarray(feats[:, :, : 2 * kv.SHORT])
    enc, cross, dec = runner("encoder" + suffix), runner("cross" + suffix), runner("decoder" + suffix)
    (hidden,) = enc(feats)
    ck, cv = cross(hidden)
    L, H, Dh, C = shapes["layers"], shapes["heads"], shapes["head_dim"], shapes["cache"]
    sk = np.zeros((L, 1, H, C, Dh), np.float32); sv = np.zeros_like(sk)
    suppress = [t for t in gen.get("suppress_tokens", [])]
    def step(tok, pos):
        outs = dec(np.array([[tok]], np.int32), np.array([pos], np.int32), sk, sv, ck, cv)
        logits = next(o for o in outs if o.shape == (1, shapes["vocab"]))
        nk, nv = [o for o in outs if o.shape == (L, 1, H, 1, Dh)]
        sk[:, :, :, pos:pos + 1] = nk; sv[:, :, :, pos:pos + 1] = nv
        return logits[0]
    langs = {int(v) for v in gen["lang_to_id"].values()}
    first = step(50258, 0)
    lang = max(langs, key=lambda i: first[i])
    tokens = [50258, lang, 50359, 50363]
    for pos, tok in enumerate(tokens[1:], start=1):
        logits = step(tok, pos)
    out = []
    while len(tokens) < C:
        logits = logits.copy(); logits[50257 + 1:] = -np.inf; logits[suppress] = -np.inf
        if not out: logits[[220, 50257]] = -np.inf
        nxt = int(logits.argmax())
        if nxt == 50257: break
        out.append(nxt); tokens.append(nxt)
        logits = step(nxt, len(tokens) - 1)
    return out, lang

def main():
    gen = json.loads((OUT / "generation_config.json").read_text())
    shapes = json.loads((OUT / "shapes.json").read_text())
    tok = WhisperTokenizer.from_pretrained(kv.WHISPER, revision=kv.WHISPER_REVISION)
    model = kv.load()
    for wav in ["../../crates/zyris-voice/tests/audio/jfk.wav", "out/clips/ko1.wav", "out/clips/ko-long.wav"]:
        ids, lang = transcribe(wav, gen, shapes)
        ours = tok.decode(ids, skip_special_tokens=True).strip()
        audio, _ = soundfile.read(wav, dtype="float32")
        feats = WhisperFeatureExtractor(feature_size=shapes["bins"])(audio, sampling_rate=16000, return_tensors="pt").input_features
        ref = tok.decode(model.generate(feats, task="transcribe")[0], skip_special_tokens=True).strip()
        print(f"{pathlib.Path(wav).name}: lang={lang}\n  tflite:       {ours}\n  transformers: {ref}", flush=True)
        assert ours == ref, "the .tflite graphs disagree with transformers"
    # The short set must read every clip it can hold exactly as the long set does.
    for wav in ["out/clips/ko1.wav", "out/clips/ko2.wav", "out/clips/en1.wav"]:
        audio, _ = soundfile.read(wav, dtype="float32")
        assert len(audio) <= 2 * kv.SHORT * 160, f"{wav} is longer than the short set"
        long_ids, _ = transcribe(wav, gen, shapes)
        short_ids, lang = transcribe(wav, gen, shapes, "-10s")
        print(f"{pathlib.Path(wav).name} short: lang={lang} {tok.decode(short_ids, skip_special_tokens=True).strip()}", flush=True)
        assert short_ids == long_ids, "the short graphs disagree with the long ones"
    # What aot.py --short compiles only if it is these bytes: checked, not merely exported.
    (OUT / "checked.json").write_text(json.dumps({f"{g}.tflite": sha256(OUT / f"{g}.tflite") for g in SHORT}))

SHORT = ("encoder-10s", "cross-10s", "decoder-10s")

def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()

if __name__ == "__main__":
    main()
