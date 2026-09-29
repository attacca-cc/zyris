"""The step decoder against transformers' logits, one token at a time.

Runs transformers' own forward over the whole sequence, then the three graphs one token at a time
with the cache written by the caller, and requires every step's logits to agree. Covers a plain
prefix and a prompted one (<|startofprev|> ...), which starts the transcript dozens of positions in,
and one step at the cache's last position.
"""
import numpy as np
import soundfile
import torch
from transformers import WhisperFeatureExtractor

import whisper_kv as kv

def features(model, wav):
    audio, rate = soundfile.read(wav, dtype="float32")
    assert rate == 16000
    return torch.from_numpy(WhisperFeatureExtractor(feature_size=model.config.num_mel_bins)(
        audio, sampling_rate=16000, return_tensors="np").input_features)

def run_steps(model, feats, tokens):
    enc, cross, dec = kv.Encoder(model), kv.Cross(model), kv.Decoder(model)
    hidden = enc(feats)
    ck, cv = cross(hidden)
    L, H, Dh = model.config.decoder_layers, model.config.decoder_attention_heads, model.config.d_model // model.config.decoder_attention_heads
    sk = torch.zeros(L, 1, H, kv.CACHE, Dh); sv = torch.zeros_like(sk)
    out = []
    for pos, tok in enumerate(tokens):
        logits, nk, nv = dec(torch.tensor([[tok]], dtype=torch.int32), torch.tensor([pos], dtype=torch.int32), sk, sv, ck, cv)
        sk[:, :, :, pos:pos + 1] = nk; sv[:, :, :, pos:pos + 1] = nv
        out.append(logits[0])
    return torch.stack(out), (sk, sv, ck, cv, dec)

def main():
    model = kv.load()
    feats = features(model, "../../crates/zyris-voice/tests/audio/jfk.wav")
    prefixes = {
        "plain": [50258, 50259, 50359, 50363],
        # <|startofprev|> " Hey Zyris" then the plain prefix: the transcript starts at position 9.
        "prompted": [50361, 1911, 1176, 88, 5714, 50258, 50259, 50359, 50363],
    }
    with torch.no_grad():
        for name, prefix in prefixes.items():
            continuation = model.generate(feats, decoder_input_ids=torch.tensor([prefix]), max_new_tokens=40)[0].tolist()
            # generate() leaves the prefix out of what it returns for a prompted call, and keeps it
            # for a plain one; the check runs the whole sequence either way.
            if continuation[:len(prefix)] == prefix:
                continuation = continuation[len(prefix):]
            generated = prefix + continuation
            reference = model(input_features=feats, decoder_input_ids=torch.tensor([generated])).logits[0]
            ours, _ = run_steps(model, feats, generated)
            diff = (ours - reference).abs().max().item()
            print(f"{name}: {len(generated)} tokens, max |logit diff| = {diff:.2e}")
            assert diff < 1e-2, f"{name}: the step decoder disagrees with transformers"
        # Every position up to the last slot, against transformers: a teacher-forced 448-token
        # sequence (the plain prefix, then text tokens), so no slot is only checked for finiteness.
        long = prefixes["plain"] + [(1000 + 7 * i) % 50000 for i in range(kv.CACHE - len(prefixes["plain"]))]
        reference = model(input_features=feats, decoder_input_ids=torch.tensor([long])).logits[0]
        ours, _ = run_steps(model, feats, long)
        diff = (ours - reference).abs().max().item()
        print(f"all {kv.CACHE} positions: max |logit diff| = {diff:.2e}")
        assert diff < 1e-2, "the step decoder disagrees with transformers somewhere up to the last slot"
        # The last slot: a step at position CACHE-1 attends to every slot and writes nothing past it.
        _, (sk, sv, ck, cv, dec) = run_steps(model, feats, prefixes["plain"])
        logits, nk, _ = dec(torch.tensor([[50363]], dtype=torch.int32), torch.tensor([kv.CACHE - 1], dtype=torch.int32), sk, sv, ck, cv)
        assert torch.isfinite(logits).all() and nk.shape[3] == 1
        print("last position: finite logits, one new slot")

if __name__ == "__main__":
    main()
