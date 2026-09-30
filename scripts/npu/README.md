# NPU model export

Builds what Android phones download to transcribe on their NPU: `openai/whisper-small` as three
static-shape graphs (`encoder`, `cross`, and a `decoder` with an explicit KV cache), checked against
`transformers`, and AOT-compiled with LiteRT 2.2.0 for every Qualcomm SoC it supports. The output is
one bundle per SoC, with a SHA-256 manifest. The phase 0 findings are on branch `spike/npu-litert`.

    bash scripts/npu/setup.sh                                   # environment, once
    cd scripts/npu
    .venv/bin/python check_torch.py                             # the step decoder against transformers
    .venv/bin/python export.py                                  # three .tflite graphs
    .venv/bin/python export.py --short                          # only the ten-second set
    .venv/bin/python check_tflite.py                            # the .tflite graphs against transformers
    bash aot.sh                                                 # per-SoC bundles and manifest.json
    bash aot.sh --short                                         # add the ten-second set to existing bundles
