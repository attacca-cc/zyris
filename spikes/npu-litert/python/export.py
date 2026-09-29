"""Hugging Face whisper to a static-shape encoder.tflite and decoder.tflite.

The decoder reruns every token so far on each step (no KV cache), through a
fixed 128-token window with a causal mask: static shapes are what NPU
compilers take, and it is the shape `onnx_stt::Runtime::next_logits` has.
"""
import json, pathlib, shutil, sys

import litert_torch
import torch
from huggingface_hub import hf_hub_download
from litert_torch.generative.export_hf.core.speech.asr_model import get_causal_mask
from litert_torch.generative.export_hf.model_ext.whisper.whisper import Whisper

TOKENS = 128

def main(model_id: str, out: pathlib.Path) -> None:
    out.mkdir(parents=True, exist_ok=True)
    whisper = Whisper(model_id)
    d_model = whisper._model.config.d_model
    bins = whisper._model.config.num_mel_bins

    features = torch.zeros(1, bins, 3000)
    litert_torch.convert(whisper.get_encoder(), (features,)).export(str(out / "encoder.tflite"))

    hidden = torch.zeros(1, 1500, d_model)
    ids = torch.zeros(1, TOKENS, dtype=torch.int32)
    mask = get_causal_mask(TOKENS)
    litert_torch.convert(whisper.get_decoder(), (hidden, ids, mask)).export(str(out / "decoder.tflite"))

    for name in ("config.json", "generation_config.json", "tokenizer.json"):
        shutil.copy(hf_hub_download(model_id, name), out / name)
    (out / "shapes.json").write_text(json.dumps({"tokens": TOKENS, "d_model": d_model, "bins": bins}))

if __name__ == "__main__":
    main(sys.argv[1], pathlib.Path(sys.argv[2]))
