# Findings: whisper on Android NPUs through LiteRT

Phase 0 of the NPU speech spec. Measured on 2026-09-29 on the test phone: Galaxy S23 Ultra, SM8550 (Snapdragon 8 Gen 2, HTP v73), Android 16. The phone is where these numbers were taken; nothing here is designed for it.

**Setup for every number below:**
- LiteRT 2.2.0 and QAIRT 2.47.0.260601.
- `openai/whisper-base` and `openai/whisper-small`, exported by `litert-torch` 0.9.4 as a static-shape pair: a 30 s encoder and a 128-token decoder with no KV cache.
- The decoding is phase 1's `onnx_stt` (log-mel, BPE, language detection, greedy decode), behind `onnx_stt::Runtime`.
- The comparison is whisper.cpp through `zyris_voice::stt::Stt`, with 4 threads (the 0.2.0 Android cap).

## 1. Does it run

**Yes.**
- A model compiled ahead of time loads and runs on the NPU with both graphs fully offloaded (`IsFullyAccelerated` true).
- The SoC where LiteRT's *on-device* compilation of whisper crashes (SM8550, google-ai-edge/LiteRT#4853) runs the *ahead-of-time* model without trouble.

**Compiled, whisper-base** (`out/whisper-base/npu/report.json`):

| SoC | Encoder ops on NPU | Decoder ops on NPU | Size |
|---|---|---|---|
| SM8450, SM8475, SM8550, SM8650, SM8750, SM8845, SM8850, SA8255, SA8295 | 353/353 | 559/559 (1 partition) | 193-196 MB |
| MT6878, MT6897, MT6983, MT6985, MT6989, MT6991, MT8171, MT8188, MT8189 | 353/353 | 551/559 (7 partitions) | 240 MB |
| MT6993 | 353/353 | 557/559 (1 partition) | 240 MB |
| MT6877, MT6879 | 301/353 | 521/559 (31 partitions) | 244 MB |
| SM8350 (HTP v68) | failed | failed | — |
| MT6853, MT6886, MT6893, MT6895 | failed | failed | — |

- **whisper-small** compiles for exactly the same 21 SoCs as base, and fails on the same five.
- **SM8350** fails because QNN rejects whisper's `GroupNorm` on that generation. Phones from 2021 and earlier stay on the CPU path.
- **Qualcomm 8-series from 2022 on** is fully offloaded, one partition each.
- **MediaTek** compiles for most recent Dimensity SoCs, but leaves a few decoder ops on the CPU. Seven partitions means seven NPU-CPU hand-offs per decoder step. That cost is unmeasured.

**Not run, and why:**
- Only the test phone's SoC was executed. Every other row above compiled but was not executed.
- LiteRT 2.2.0's runtime bundle carries no MediaTek dispatch library. Where a MediaTek phone gets one (the device's vendor partition, or NeuroPilot's runtime) is an open item for phase 2.

**Failure behaviour:**
- **SoC with no compiled model** (checked with `NPU_BENCH_SOC=EXYNOS2400`): the bench says so and runs on the CPU, with a correct transcript.
- **NPU unreachable** (the DSP library path pointed at an empty directory): the graphs still *open* and report `fully accelerated`. The failure surfaces only at the first run (status 3). Every clip then reports the error, and none reports a transcript.
  - **Phase 2 must therefore run one warm-up inference at load**, and treat a failed warm-up as "no NPU".

## 2. Speed

Warm milliseconds per clip (`out/results/`). The clips are Supertonic TTS (`ko1`, `ko2`, `en1`, 3-4 s; `ko-long`, ~10 s) and `jfk.wav` (11 s).

| Clip | base NPU | base LiteRT CPU | base whisper.cpp | small NPU | small whisper.cpp |
|---|---|---|---|---|---|
| en1 | 1,661 | 6,902 | 2,376 | 3,554 | 55,239 |
| ko1 | 1,827 | 9,490 | 3,245 | 3,420 | 61,990 |
| ko2 | 1,792 | 11,430 | 3,516 | 3,786 | 63,282 |
| jfk | 2,995 | 15,595 | 4,459 | 5,996 | 55,571 |
| ko-long | 4,651 | 29,629 | 4,562 | 10,076 | 55,382 |

**Where the NPU time goes** (per turn):

| Model | Encoder, 30 s window | Decoder, per token | Log-mel and the rest |
|---|---|---|---|
| base | 330-340 ms | 82-86 ms | 240-850 ms |
| small | 966-977 ms | 167-171 ms | 275-850 ms |

**The decoder dominates, and it is the export's fault, not the NPU's.**
- With no KV cache, every step recomputes all 128 positions.
- Every step also copies the logits for all 128 positions back to the CPU: 128 × 51,865 floats, 26 MB per step.
- A stateful decoder with a KV cache, returning the last position only, removes both costs. `litert-torch` exports one: `stateful_after`, the `decode_1` signature. Qualcomm's own whisper export reports about 2 ms per token on an 8 Elite.

**Cold start:** loading both graphs took 488-812 ms for base and about 700 ms for small. The first turn after loading was as fast as the ones after it, because there is no on-device compile. A first run after a reboot was not measured.

**Even this naive export beats whisper.cpp on the same phone:**
- base: 1.3-1.8× faster;
- small: 6-17× faster. whisper.cpp small takes 55-63 s a turn on this phone's CPU, which is unusable.

## 3. Accuracy

Transcripts against the sentence spoken (full lines in `out/results/`):

| Clip | base NPU | base CPU (LiteRT, whisper.cpp) | small NPU | small whisper.cpp |
|---|---|---|---|---|
| en1, jfk, ko1 | exact | exact | exact | exact |
| ko2 | "일정**지에서**" ✗ | exact | exact | exact |
| ko-long | cut off after "각 이슈의 이슈의" ✗ | "기토부", "세계만/세 개만" | "기터브", "초알도" | "기터브", otherwise exact |

- **The NPU loses accuracy on base.** The same graph on LiteRT's CPU (fp32) gets ko2 and ko-long right. The HTP computes in fp16, and the Qualcomm compiler options offer no fp32 path. So this is the price of the NPU, not of the export.
- **Small absorbs it.** Small on the NPU is at least as good as base on any CPU, and nearly matches small on whisper.cpp: two misspelled words in the long Korean sentence.

## 4. The UI while it runs

Zyris' Conversation screen was scrolled by `adb shell input swipe` for 20 s (`jank.sh`), and `dumpsys gfxinfo` measured the frames.

| While | Janky frames |
|---|---|
| idle | 0.61% (4 of 651); 0.00% (0 of 1,161) on a second run |
| whisper-small on the NPU | 0.00% (0 of 1,144), with the bench confirmed running at both ends (same process) |
| whisper-base on LiteRT's CPU | 0.49% (4 of 810), with the bench confirmed running at both ends |

- **The NPU does not touch the UI.** This is the opposite of the Adreno GPU path reverted in 0.2.0, where ggml-vulkan shared the GPU with the compositor.
- CPU load at this level did not cause jank either. The stutter users saw was GPU contention, which the NPU avoids by construction.

## 5. Shipping cost for a sideloaded APK

Zyris is sideloaded from GitHub releases, not installed from Google Play. So Play's delivery of runtime modules and AI packs per device group does not apply: every byte is ours to deliver.

| Piece | Size |
|---|---|
| `libLiteRt.so` (arm64) | 5.5 MB |
| QNN, shared by all generations (`libQnnHtp.so`, `libQnnSystem.so`) | 7.6 MB |
| QNN, per HTP generation (skel, stub, LiteRT dispatch) | ~12.5 MB × 5 generations (v69, v73, v75, v79, v81) |
| Compiled whisper-base, per SoC | 193-196 MB (Qualcomm), 240-244 MB (MediaTek) |
| Compiled whisper-small, per SoC | 603-630 MB (SM8550: 246 MB encoder + 392 MB decoder) |

**Recommendation: a hybrid.**
- **In the APK:** `libLiteRt.so` and the shared QNN libraries, about 13 MB.
- **Downloaded at first launch:** the phone's generation of skel and stub (about 12 MB) and the phone's compiled model. This is exactly how the whisper models are already fetched: pinned by URL and SHA-256, one file set per SoC family.
- Bundling every generation would add about 70 MB to every APK for libraries that only one generation uses. Bundling models is out of the question at these sizes.

## 6. Decision

1. **The Android runtime is LiteRT.** An ahead-of-time compiled whisper runs fully on the NPU on the test phone, and compiles for Qualcomm from 2022 on and for most recent MediaTek SoCs, all from one API. The ONNX Runtime QNN fallback was not needed.
2. **The NPU default is whisper-small, not base.**
   - The HTP's fp16 costs base its Korean.
   - Small on the NPU is as accurate as whisper.cpp small, and already 6-17× faster than it, with no KV cache.
   - Base stays the CPU default.
3. **Phase 2 exports a stateful decoder** (KV cache, last-position logits). The decoder's 85-170 ms per token is almost all recomputation and copying. The target is the encoder plus a few milliseconds per token.
4. **Phase 1's `Runtime` trait held.** `LiteRt` implements `encode` and `next_logits` with no change to `onnx_stt`, and the transcripts came out through `OnnxStt` unchanged.
   - A stateful decoder will want the trait to say when a turn starts, so the KV cache can be reset. `encode` is already that point, so no new method is needed.
5. **Phase 2 warms up at load**, and treats a failed warm-up as "no NPU" (section 1).
6. **Distribution is the hybrid in section 5.** The per-SoC files are keyed by `ro.soc.model` through one table, as `src/soc.rs` does.
