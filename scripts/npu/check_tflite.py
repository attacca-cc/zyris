"""Greedy decode with the three .tflite graphs, language detected, against transformers.generate."""
import json, pathlib

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

def transcribe(wav, gen, shapes):
    audio, rate = soundfile.read(wav, dtype="float32"); assert rate == 16000
    feats = WhisperFeatureExtractor(feature_size=shapes["bins"])(audio, sampling_rate=16000, return_tensors="np").input_features.astype(np.float32)
    enc, cross, dec = runner("encoder"), runner("cross"), runner("decoder")
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

if __name__ == "__main__":
    main()
