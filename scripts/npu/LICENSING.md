# What the QAIRT licence lets Zyris ship

Read on 2026-09-30 from `LICENSE.pdf` in the Qualcomm AI Runtime Community SDK 2.47.0.260601,
titled *"Terms and Conditions of Use — AI Stack License"*. `setup.sh` downloads it into
`scripts/npu/out/qairt/`. This is a reading for engineering decisions, not legal advice.

## The clause that decides it

Section 1, *Grant of License*. QTI grants a non-exclusive, non-transferable, **revocable** licence to:

> (iv) distribute and sublicense the Software solely in object code format and as incorporated in
> Your software application […] For the avoidance of doubt, nothing herein grants You a license to
> distribute or sublicense the Software on a standalone basis.

## What that means for phase 2

| What | Allowed? |
|---|---|
| `libQnnHtp.so`, `libQnnSystem.so`, the per-generation `libQnnHtpV<N>Stub.so` and `libQnnHtpV<N>Skel.so`, **inside the Zyris APK** | **Yes.** Object code, incorporated in our application. |
| The same libraries as **separate downloads** (GitHub release assets, a bundle fetched at first use) | **No.** That is distribution on a standalone basis. |
| **Compiled models** (the `.tflite` graphs the AOT compiler writes from our whisper export) | Not the Software; they are our output. |
| LiteRT's `libLiteRtDispatch_Qualcomm.so` (Apache 2.0, from the LiteRT release) | Yes, under Apache 2.0. |

## Also in the licence

- **The licence is revocable** (Section 1).
- **Unacceptable-risk and high-risk uses** are excluded or advised against (Section 2). Examples are biometric identification, law enforcement and critical infrastructure. Speech transcription for a personal assistant is neither.
- **Export and sanctions law** applies to the Software and anything incorporating it.
- **We indemnify QTI** for our distribution, including posting apps on download sites (Section 7).
- **`QNN_NOTICE.txt` and `NOTICE.txt`** carry third-party notices for components inside the libraries. They ship with the APK's licence notices.

## Consequence

- **The APK carries every HTP generation's skel and stub** (v69, v73, v75, v79, v81), not just the phone's.
  - Measured sizes: about 8 MB shared and about 62 MB per-generation; with `libLiteRt.so`, about 76 MB added to the APK.
  - This exceeds the phase 2 spec's 15 MB goal and needs the maintainer's decision.
- **Model bundles hold only the compiled graphs and the model's JSON files.** `aot.py` no longer copies QNN libraries into them.
