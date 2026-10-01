"""The three graphs as .tflite, beside the files the app's decoder reads."""
import json, pathlib, shutil, sys

import litert_torch
import torch
from huggingface_hub import hf_hub_download

import whisper_kv as kv

def main(out=pathlib.Path("out/whisper-small"), short_only=False):
    out.mkdir(parents=True, exist_ok=True)
    model = kv.load()
    c = model.config
    L, H, D = c.decoder_layers, c.decoder_attention_heads, c.d_model
    Dh = D // H
    feats = torch.zeros(1, c.num_mel_bins, 3000)
    hidden = torch.zeros(1, 1500, D)
    cross_k = torch.zeros(L, 1, H, 1500, Dh)
    self_k = torch.zeros(L, 1, H, kv.CACHE, Dh)
    tok, pos = torch.zeros(1, 1, dtype=torch.int32), torch.zeros(1, dtype=torch.int32)
    with torch.no_grad():
        if not short_only:
            litert_torch.convert(kv.Encoder(model), (feats,)).export(str(out / "encoder.tflite"))
            litert_torch.convert(kv.Cross(model), (hidden,)).export(str(out / "cross.tflite"))
            litert_torch.convert(kv.Decoder(model), (tok, pos, self_k, self_k.clone(), cross_k, cross_k.clone())).export(str(out / "decoder.tflite"))
        # The ten-second set (whisper_kv.SHORT positions), beside the thirty-second one.
        short_k = torch.zeros(L, 1, H, kv.SHORT, Dh)
        litert_torch.convert(kv.ShortEncoder(model, kv.SHORT), (torch.zeros(1, c.num_mel_bins, 2 * kv.SHORT),)).export(str(out / "encoder-10s.tflite"))
        litert_torch.convert(kv.Cross(model), (torch.zeros(1, kv.SHORT, D),)).export(str(out / "cross-10s.tflite"))
        litert_torch.convert(kv.Decoder(model), (tok, pos, self_k, self_k.clone(), short_k, short_k.clone())).export(str(out / "decoder-10s.tflite"))
    for name in ("generation_config.json", "tokenizer.json"):
        shutil.copy(hf_hub_download(kv.WHISPER, name, revision=kv.WHISPER_REVISION), out / name)
    (out / "shapes.json").write_text(json.dumps({"cache": kv.CACHE, "layers": L, "heads": H, "head_dim": Dh,
        "d_model": D, "bins": c.num_mel_bins, "vocab": c.vocab_size}))
    for f in sorted(out.glob("*.tflite")):
        print(f.name, f.stat().st_size)

if __name__ == "__main__":
    main(short_only="--short" in sys.argv)
