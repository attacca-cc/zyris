# What the QAIRT licence lets Zyris ship

Read on 2026-09-29 from `LICENSE.pdf` in the Qualcomm AI Runtime Community SDK 2.47.0.260601,
titled *"Terms and Conditions of Use — AI Stack License"*. `setup.sh` downloads it into
`scripts/npu/out/qairt/`, pinned by SHA-256. This is a reading for engineering decisions, not
legal advice; a lawyer should confirm it before the libraries ship.

## The clause that decides it

Section 1, *Grant of License*. QTI grants a non-exclusive, non-transferable, **revocable** licence to:

> (iv) distribute and sublicense the Software solely in object code format and as incorporated in
> Your software application […] For the avoidance of doubt, nothing herein grants You a license to
> distribute or sublicense the Software on a standalone basis.

## What that means for phase 2

| What | Allowed? |
|---|---|
| `libQnnHtp.so`, `libQnnSystem.so` and each generation's `libQnnHtpV<N>Stub.so` and `libQnnHtpV<N>Skel.so`, **inside the Zyris APK** | **Yes.** Object code, incorporated in our application. |
| The same libraries as **separate downloads** (GitHub release assets, a bundle fetched at first use) | **No.** That is distribution on a standalone basis. |
| The same libraries **committed to this repository** | **No.** That is not "incorporated in Your software application" either. CI, and anyone rebuilding Zyris, fetch QAIRT themselves (`setup.sh`). |
| **Compiled models** (the `.tflite` graphs LiteRT's AOT compiler writes from our whisper export, which embed QNN context binaries made by QTI's tools) | **Assumed allowed, not read from the text.** The licence neither names nor excludes compiler outputs. Section 3 speaks only of "Modifications". Treat this as an open question for the lawyer. |
| LiteRT's `libLiteRtDispatch_Qualcomm.so` (Apache 2.0, from the LiteRT release) | Yes, under Apache 2.0. |

## Also in the licence

- **Termination, Section 8.** QTI may end the agreement at any time, without cause, on notice, and then every copy of the Software must be deleted or destroyed.
  - For us, that means pulling every published APK that carries the libraries.
  - It is the largest risk of shipping them, and the reason the app must keep working without them (the CPU path).
- **Prohibited and high-risk uses, Section 2.d-2.e.** Examples are biometric identification, law enforcement and critical infrastructure. Speech transcription for a personal assistant is neither.
- **Liability, Section 7.** QTI's liability is capped at $100.
- **Indemnification, Section 9.** We indemnify QTI for our distribution, including posting apps on download sites.
- **Notices, Section 10.h.** The Notice File must not be removed or altered.
  - `QNN_NOTICE.txt` and `NOTICE.txt`, whose third-party notices include GPLv2 and LGPL components, ship unaltered with any APK carrying the libraries.
- **Export and sanctions law** applies to the Software and anything incorporating it.

## Consequences for an Apache-2.0 app released on GitHub

- **A carve-out in the licence notices.** The APK's licence notices must say that the QNN libraries are QTI's proprietary code under this agreement, not Apache 2.0.
- **F-Droid-style distribution is ruled out** for an APK carrying them, because it requires everything to be free software.
- **Every HTP generation's skel and stub go into the APK** (v69, v73, v75, v79, v81), not just the phone's.
  - Measured sizes: about 8 MB shared and about 62 MB per-generation; with `libLiteRt.so`, about 76 MB added.
  - This exceeds the phase 2 spec's 15 MB goal and needs the maintainer's decision.
  - One option the licence allows is one APK per HTP generation, each still "incorporated in the application".
- **Model bundles hold only the compiled graphs and the model's JSON files.** `aot.py` never copies a QNN library into them.
