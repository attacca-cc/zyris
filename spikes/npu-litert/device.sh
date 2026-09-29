#!/usr/bin/env bash
# push [model...] | run <model> [npu|cpu] [extra args] | clean — against the phone adb sees.
# Run through android-env.sh, which puts adb and the NDK on the path.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd); D=/data/local/tmp/zyris-npu
case "$1" in
  push)
    shift
    adb shell mkdir -p $D
    adb push "$here/target/aarch64-linux-android/release/npu-bench" $D/ >/dev/null
    adb push "$LITERT_SDK/aar/jni/arm64-v8a/libLiteRt.so" $D/ >/dev/null
    adb push "$ANDROID_NDK/toolchains/llvm/prebuilt/linux-x86_64/sysroot/usr/lib/aarch64-linux-android/libc++_shared.so" $D/ >/dev/null
    adb push "$here/qairt" $D/ >/dev/null
    adb push "$here/out/clips" $D/ >/dev/null
    # Only this phone's compiled graphs go over, beside the CPU pair and the model's text files.
    soc=$(adb shell getprop ro.soc.model | tr -d '\r')
    for m in "${@:-whisper-base}"; do
      adb shell mkdir -p $D/$m/npu
      for f in encoder.tflite decoder.tflite config.json generation_config.json tokenizer.json shapes.json; do adb push "$here/out/$m/$f" $D/$m/ >/dev/null; done
      [ -d "$here/out/$m/npu/$soc" ] && adb push "$here/out/$m/npu/$soc" $D/$m/npu/ >/dev/null
    done
    ggml=${ZYRIS_WHISPER_MODEL:-$HOME/.cache/zyris/models/ggml-base.bin}
    [ -f "$ggml" ] && adb push "$ggml" $D/ggml-base.bin >/dev/null
    adb shell ls -R $D | head -40 ;;
  run)
    # The same table as src/soc.rs: the phone's SoC decides the HTP generation, never the script.
    soc=$(adb shell getprop ro.soc.model | tr -d '\r')
    case "$soc" in
      SM8450|SM8475) htp=v69 ;; SM8550) htp=v73 ;; SM8650) htp=v75 ;; SM8750) htp=v79 ;;
      SM8845|SM8850) htp=v81 ;; *) htp=none ;;
    esac
    libs=$D; [ "$htp" != none ] && libs="$D:$D/qairt/$htp"
    adb shell "cd $D && export LD_LIBRARY_PATH=$libs ADSP_LIBRARY_PATH='$D/qairt/$htp;/vendor/lib/rfsa/adsp;/vendor/dsp/cdsp;/system/lib/rfsa/adsp' ${NPU_BENCH_SOC:+NPU_BENCH_SOC=$NPU_BENCH_SOC} && ./npu-bench --model-dir $D/$2 --clips $D/clips --accelerator ${3:-npu} ${4:-}" ;;
  clean) adb shell rm -rf $D ;;
esac
