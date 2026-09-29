"""Greedy decode of jfk.wav with the .tflite pair, against transformers' own."""
import json, pathlib, sys

import numpy as np
import soundfile
import torch
from ai_edge_litert.interpreter import Interpreter
from transformers import WhisperFeatureExtractor, WhisperForConditionalGeneration, WhisperTokenizer

def main(model_id: str, out: pathlib.Path, wav: str) -> None:
    audio, rate = soundfile.read(wav, dtype="float32")
    assert rate == 16000
    shapes = json.loads((out / "shapes.json").read_text())
    features = WhisperFeatureExtractor(feature_size=shapes["bins"])(audio, sampling_rate=16000, return_tensors="np").input_features

    encoder = Interpreter(model_path=str(out / "encoder.tflite"))
    run_encoder = encoder.get_signature_runner()
    (encoder_input,) = run_encoder.get_input_details().keys()
    hidden = list(run_encoder(**{encoder_input: features}).values())[0]

    decoder = Interpreter(model_path=str(out / "decoder.tflite"))
    run_decoder = decoder.get_signature_runner()
    names = sorted(run_decoder.get_input_details().keys())  # args_0, args_1, args_2: forward's order
    print("decoder inputs:", {k: (v["shape"].tolist(), str(v["dtype"])) for k, v in run_decoder.get_input_details().items()})
    n = shapes["tokens"]
    mask = np.where(np.tril(np.ones((n, n), dtype=bool)), 0.0, -np.inf).astype(np.float32)[None, None]
    tokens = [50258, 50259, 50359, 50363]  # sot, en, transcribe, notimestamps
    while len(tokens) < n:
        ids = np.zeros((1, n), dtype=np.int32); ids[0, :len(tokens)] = tokens
        inputs = dict(zip(names, (hidden, ids, mask)))  # encoder_hidden_states, input_ids, attention_mask
        logits = list(run_decoder(**inputs).values())[0][0, len(tokens) - 1].copy()
        logits[50258:] = -np.inf
        nxt = int(logits.argmax())
        if nxt == 50257:
            break
        tokens.append(nxt)
    tokenizer = WhisperTokenizer.from_pretrained(model_id)
    ours = tokenizer.decode(tokens, skip_special_tokens=True)

    model = WhisperForConditionalGeneration.from_pretrained(model_id)
    ref_ids = model.generate(torch.from_numpy(features), language="en", task="transcribe")
    reference = tokenizer.decode(ref_ids[0], skip_special_tokens=True)
    print("tflite:      ", ours)
    print("transformers:", reference)
    assert ours.strip() == reference.strip(), "the .tflite pair disagrees with transformers"

if __name__ == "__main__":
    main(sys.argv[1], pathlib.Path(sys.argv[2]), sys.argv[3])
