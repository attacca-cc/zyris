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

## Two graph sets

Every bundle has a thirty-second set (`encoder`, `cross`, `decoder`; in `npu-models-1`) and a
ten-second one (`*-10s`; in `npu-models-2`). The app runs speech of up to ten seconds on the second.

Measured in the app on an S23 (SM8550), 2026-09-30:

| | ten-second set | thirty-second set |
|---|---|---|
| encoder and cross | 185-206 ms | ~1000 ms |
| decoder step | 51-58 ms | ~91 ms |

- **A short request:** transcribed 2.05 s after it ended, against 3.4-4.2 s on the thirty-second set.
- **Memory:** the app's total PSS was 1.23 GB with the ten-second set open, and 1.88 GB with both open. So the thirty-second set opens only on the first speech longer than ten seconds.
