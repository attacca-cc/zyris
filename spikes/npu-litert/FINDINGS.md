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
| MT6877, MT6879 | 301/353 (53 partitions) | 521/559 (31 partitions) | 244 MB |
| SM8350 (HTP v68) | failed | failed | — |
| MT6853, MT6886, MT6893, MT6895 | failed | failed | — |

- **whisper-small** compiles for exactly the same 21 SoCs as base, and fails on the same five.
- **SM8350 fails.** The compile log showed QNN rejecting whisper's `GroupNorm` on that generation. That log was a temporary file and is gone.
- **MT6853, MT6886, MT6893 and MT6895 also fail**, including 2022-2023 SoCs; their causes were not kept.
- **"Qualcomm" here means only the flagship SoCs LiteRT 2.2.0 lists.** Non-flagships such as the 8s Gen 3 (SM8635) are not in its target list.
- **Qualcomm 8-series from 2022 on** is fully offloaded, one partition each.
- **whisper-small**: Qualcomm fully offloaded as for base. MediaTek decoders at 1085/1099 in 13 partitions. MT6877 and MT6879 at 577/677 encoder ops (101 partitions) and 1025/1099 decoder ops (61 partitions).
- **MediaTek** compiles for most recent Dimensity SoCs, but leaves a few decoder ops on the CPU. Seven partitions means seven NPU-CPU hand-offs per decoder step. That cost is unmeasured.

**Not run, and why:**
- Only the test phone's SoC was executed. Every other row above compiled but was not executed.
- LiteRT 2.2.0's runtime bundle carries no MediaTek dispatch library. Where a MediaTek phone gets one (the device's vendor partition, or NeuroPilot's runtime) is an open item for phase 2.

**All of this ran as the shell user, from `/data/local/tmp`, in a separate process.** An app runs as `untrusted_app`, under a different SELinux domain. Whether it can reach FastRPC, and load a skel library from its own data directory through `ADSP_LIBRARY_PATH`, was not tested. The jank measurement also had inference in another process than the UI, whereas the product runs it in-process. Both are open items for phase 2.

**Failure behaviour:**
- **SoC with no compiled model** (checked with `NPU_BENCH_SOC=EXYNOS2400` on the real SM8550): the bench says so and transcribes on the CPU correctly.
  - **But it still gave LiteRT the dispatch directory.** LiteRT found five dispatch libraries, loaded one (v75's, on a v73 phone), and opened the DSP.
  - On a real Exynos or MediaTek phone, this path is untested.
  - **Phase 2 must create the CPU environment with no dispatch directory.**
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

**The decoder dominates. Three costs are inside its per-token number, and they were not separated:**
1. **The export:** with no KV cache, every step recomputes all 128 positions.
2. **The export's output:** every step returns logits for all 128 positions, 128 × 51,865 floats, 26 MB.
3. **The bench itself:** `Graph::run` creates, registers with the DSP and destroys every buffer on every call, about 30 MB. The QNN log shows four `QnnMem_register` calls before each execution.

**The "rest" column is mislabelled.** It grows with the number of decoder calls (239 ms at 13 calls, 732 ms at 42), because the 26 MB logits conversion sits outside the timer.

**Phase 2 needs a decoder with an explicit KV cache that returns one position's logits, and buffers allocated once per graph.** `litert-torch`'s `stateful_after` is meant for the LiteRT-LM runtime and does not export such a decoder, so phase 2 writes its own. What per-token time that reaches on this phone is unmeasured. Qualcomm's 2 ms per token was measured on an 8 Elite, with Qualcomm's own export.

**Cold start:**
- Loading both graphs took 444-867 ms for base, and 701-1,429 ms for small (1,429 ms the first time).
- The first turn after loading was as fast as later ones. Only the first sorted clip's `cold_ms` is a true first turn.
- A first run after a reboot was not measured.

**Against whisper.cpp on the same phone:**
- **base on the NPU is 1.4-2.0× faster on the short clips, and no faster on `ko-long`** (4,651 ms against 4,562 ms, with a truncated transcript).
- **small on the NPU is 5.5-18× faster than whisper.cpp small.** That comparison is weak, because whisper.cpp small's 55-63 s is suspect:
  - It is 12-26× base's time on this phone, against about 4× on the desktop.
  - It was flat across 3-11 s clips.
  - It rose in run order.
  - It ran twice per clip, for about ten minutes of sustained four-core load, in a process that also held the NPU graphs. Thermal throttling is the likely reason.
- whisper.cpp base was measured right after LiteRT's all-core CPU runs, not beside the NPU run as planned.
- **Both whisper.cpp numbers need a fresh process on a cool phone before any ratio is quoted.**

## 3. Accuracy

Transcripts against the sentence spoken (full lines in `out/results/`):

| Clip | base NPU | base CPU (LiteRT, whisper.cpp) | small NPU | small whisper.cpp |
|---|---|---|---|---|
| en1, jfk, ko1 | exact | exact | exact | exact |
| ko2 | "일정**지에서**" ✗ | exact | exact | exact |
| ko-long | cut off after "각 이슈의 이슈의" ✗ | "기토부", "세계만/세 개만" | "기터브", "초알도" | "기터브", otherwise exact |

- **Punctuation is ignored in the comparison.** Small on the NPU dropped ko1's final full stop, and that counts as exact.
- **base on the NPU lost `ko2` and truncated `ko-long`.** base on LiteRT's CPU got `ko2` right, and `ko-long` whole but with three errors ("기토부", "세계만", "요약해져"), the same ones whisper.cpp base made.
- **fp16 is the likely cause, but it was not isolated.** There was no fp16 run of the same graph on the CPU. The NPU was also fed an `-inf` attention mask, which is a known fp16 hazard; exports normally use a large finite negative.
- **small on the NPU is close to whisper.cpp small**, with one extra error ("초알도") in `ko-long`.

## 4. The UI while it runs

Zyris' Conversation screen was scrolled by `adb shell input swipe` for 20 s (`jank.sh`), and `dumpsys gfxinfo` measured the frames.

| While | Janky frames |
|---|---|
| idle | 0.61% (4 of 651); 0.00% (0 of 1,161) on a second run |
| whisper-small on the NPU | 0.00% (0 of 1,144), with the bench confirmed running at both ends (same process) |
| whisper-base on LiteRT's CPU | 0.49% (4 of 810), with the bench confirmed running at both ends |

- **Running the NPU in another process did not cause jank.**
- **The runs are small and not strictly comparable:**
  - idle noise (0.61% against 0.00% across two runs) is as large as any difference;
  - frame counts differed;
  - the inference ran outside the app;
  - the CPU case was LiteRT's XNNPACK with default threads, not whisper.cpp's four busy threads.
- **That the stutter users saw in 0.2.0 was GPU contention is consistent with this, but was not measured here.**

## 5. Shipping cost for a sideloaded APK

Zyris is sideloaded from GitHub releases, not installed from Google Play. So Play's delivery of runtime modules and AI packs per device group does not apply: every byte is ours to deliver.

| Piece | Size |
|---|---|
| `libLiteRt.so` (arm64) | 5.5 MB |
| Shared by all generations: `libQnnHtp.so`, `libQnnSystem.so`, `libLiteRtDispatch_Qualcomm.so` (byte-identical across generations) | 8.0 MB |
| Per HTP generation: skel and stub | ~12 MB (13 MB for v81) × 5 generations; about 62 MB for all five |
| Compiled whisper-base, per SoC | 193-196 MB (Qualcomm), 240-244 MB (MediaTek) |
| Compiled whisper-small, per SoC | 603-630 MiB (SM8550: 235 + 374 MiB) |

**Recommendation: a hybrid.**
- **In the APK:** `libLiteRt.so` and the shared libraries, about 13.5 MB.
- **Downloaded when the NPU is chosen:** the phone's generation's skel and stub (about 12 MB), and the phone's compiled model.
- **Only if an app can load a downloaded skel through `ADSP_LIBRARY_PATH`**, which is untested (section 1). If it cannot, the skels go in the APK too. This is exactly how the whisper models are already fetched: pinned by URL and SHA-256, one file set per SoC family.
- Bundling every generation would add about 62 MB to every APK for libraries that only one generation uses. Bundling models is out of the question at these sizes.

## 6. Decision

1. **The Android runtime is LiteRT.** An ahead-of-time compiled whisper runs fully on the NPU on the test phone, and compiles for Qualcomm from 2022 on and for most recent MediaTek SoCs, all from one API. The ONNX Runtime QNN fallback was not needed.
2. **whisper-small is the NPU model, where it is measured fast and accurate enough.**
   - On SM8550, base on the NPU lost Korean, and small did not.
   - small on the NPU is faster than whisper.cpp small, but the ratio awaits a clean re-measurement.
   - Other generations (v69's slower HTP) and the partitioned MediaTek graphs are unmeasured, and so is holding 600 MiB in RAM on a 6-8 GB phone.
   - Re-decide per HTP generation once phase 2's decoder exists. Base stays the CPU default.
3. **Phase 2 exports a decoder with an explicit KV cache**, returning one position's logits, with buffers allocated once per graph.
   - The 85-170 ms per token mixes the export's recomputation, its 26 MB output and the bench's per-call buffers. The gain is expected, but its size is unmeasured.
   - **The cache is 448 positions, not 128.** The spike's 128-token window leaves 124 transcript tokens. A 10 s Korean clip already used 49. A 30-35 s turn, or any `transcribe_expecting` prompt (up to 224 tokens), would not fit.
4. **Phase 1's `Runtime` trait held.** `LiteRt` implements `encode` and `next_logits` with no change to `onnx_stt`, and the transcripts came out through `OnnxStt` unchanged.
   - A stateful decoder will want the trait to say when a turn starts, so the KV cache can be reset. `encode` is already that point, so no new method is needed.
5. **Phase 2 warms up at load**, and treats a failed warm-up as "no NPU" (section 1).
6. **Distribution is the hybrid in section 5, if an app can load a downloaded skel.** Otherwise the skels go in the APK. The per-SoC files are keyed by `ro.soc.model` through one table. `src/soc.rs` left out SA8255 and SA8295, which compiled.
7. **On the CPU path, no dispatch directory** (section 1).
