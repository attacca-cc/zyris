# Gate: can an app reach the NPU?

Run on 2026-09-29 on the test phone: Galaxy S23 Ultra (SM-S918N), SM8550, Android 16, security patch 2026-04-05. The bench is the phase 0 spike's `npu-bench` (branch `spike/npu-litert`, commit "the dispatch directory can be overridden"), with whisper-base compiled for SM8550. Commands: `scripts/npu/gate/gate.sh`.

## Result: inconclusive for an app. `run-as` cannot stand in for one.

| Run | Context | Result |
|---|---|---|
| `shell-control` | shell user, `/data/local/tmp` | **NPU works**: `accelerator=npu`, both graphs fully on the NPU, `ko1` transcribed exactly |
| `files-dir` | `run-as cc.attacca.npugate`, skel in the app's files directory | QNN `Failed to create device handle` |
| `native-lib-dir` | the same, skel in the APK's native library directory | QNN `Failed to create device handle` |
| `files-dir` with `/vendor/lib64` added to `LD_LIBRARY_PATH` | the same | the DSP refuses: `remote_handle_control_domain failed … on domain 3 (errno Permission denied)` |

## Why

1. **`run-as` does not give an app's SELinux domain.** It gave `u:r:runas_app:s0:…`, not `untrusted_app`. The plan assumed it would.
2. **The first failure is the linker, not the skel.**
   - Logged as `dlopen failed: library "libcdsprpc.so" not found … in namespace (default)`, raised from the stub library.
   - An app gets Qualcomm's `libcdsprpc.so` by declaring `<uses-native-library android:name="libcdsprpc.so">` in its manifest (Android 12+).
   - A process started by `run-as` is not started by zygote as that app, so the declaration does not apply.
   - The shell user's `/data/local/tmp` has an unrestricted linker section, which is why the spike never met this.
3. **Past the linker, the compute DSP refuses `runas_app`** with `Permission denied` on domain 3 (the cDSP).

   Sideloaded *apps* reach it: a working example on a Snapdragon 8 Elite Gen 5, itsallgoody/onnxruntime-qnn-snapdragon-8-elite-gen5, used four things:
   - `uses-native-library libcdsprpc.so`;
   - `ADSP_LIBRARY_PATH`;
   - legacy packaging;
   - the skel among the app's native libraries.

## What decides the design now

- **The QAIRT licence** (`LICENSING.md`) allows the QNN libraries only inside the app. So the skel goes in the APK's native library directory whatever this gate had shown. The files-directory question no longer matters.
- **Whether an app process reaches the NPU is still open.** It is plan 2B's first task, run in a real app, not `run-as`:
  - a debug build of Zyris with `uses-native-library libcdsprpc.so`;
  - legacy packaging (`useLegacyPackaging = true`), so the skel exists as a file;
  - the skels in `jniLibs`;
  - `ADSP_LIBRARY_PATH` set to the native library directory before LiteRT starts.
