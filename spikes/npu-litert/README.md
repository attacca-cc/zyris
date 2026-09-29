# Spike: whisper on Android NPUs through LiteRT

Phase 0 of `docs/superpowers/specs/2026-09-29-npu-speech-design.md`. It answers five questions, with numbers:

1. Does a whisper compiled ahead of time for a phone's NPU load and run?
2. How fast is it?
3. Is the Korean as accurate as whisper.cpp's?
4. Does the UI stay smooth while it runs?
5. What does it cost a sideloaded APK?

It is built for every SoC LiteRT 2.2.0 supports. The Galaxy S23 Ultra (SM8550) is the phone it was measured on, not a target.

**This branch is never merged.** What leaves it is `FINDINGS.md` and the spec's updated decisions.

## Reproduce

```bash
# 1. Python environment (this machine runs manylinux wheels through nix-ld)
nix-shell -p uv --run 'uv venv .venv --python 3.12 && uv pip install --python .venv/bin/python litert-torch==0.9.4 ai-edge-litert==2.2.0 ai-edge-litert-sdk-qualcomm==2.2.0 ai-edge-litert-sdk-mediatek==2.2.0 transformers soundfile'

# 2. Export whisper to a static-shape tflite pair, and check it against transformers
.venv/bin/python python/export.py openai/whisper-base out/whisper-base
.venv/bin/python python/check.py openai/whisper-base out/whisper-base ../../crates/zyris-voice/tests/audio/jfk.wav
.venv/bin/python python/export.py openai/whisper-small out/whisper-small
.venv/bin/python python/check.py openai/whisper-small out/whisper-small ../../crates/zyris-voice/tests/audio/jfk.wav
```

`litert_torch`'s `Whisper` wrapper converted both graphs with its defaults; `override_transformers` was not needed. The decoder's inputs are `args_0` (encoder hidden states `[1, 1500, d_model]` f32), `args_1` (input ids `[1, 128]` i32) and `args_2` (causal mask `[1, 1, 128, 128]` f32). Its output is the logits for all 128 positions.
