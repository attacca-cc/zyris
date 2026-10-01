#!/usr/bin/env bash
# build | install | shell-control | files-dir | native-lib-dir | clean
# Whether an app process (not the shell user) reaches the NPU, with the Hexagon skel in the app's
# files directory (a download) or in its native library directory (inside the APK).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd); npu=$(cd "$here/.." && pwd)
SDK=$(ls -d /nix/store/*-androidsdk/libexec/android-sdk | head -1); BT=$SDK/build-tools/36.0.0
spike=${SPIKE:-$npu/../../spikes/npu-litert}
P=cc.attacca.npugate; T=/data/local/tmp/npugate; F=/data/data/$P/files
soc=$(adb shell getprop ro.soc.model | tr -d '\r')
case "$soc" in SM8450|SM8475) v=69;; SM8550) v=73;; SM8650) v=75;; SM8750) v=79;; SM8845|SM8850) v=81;; *) echo "no HTP for $soc"; exit 1;; esac
skel="$npu/out/qairt/hexagon-v$v/unsigned/libQnnHtpV${v}Skel.so"; stub="$npu/out/qairt/aarch64-android/libQnnHtpV${v}Stub.so"
bench() { # <ld path> <adsp path> <dispatch dir>
  echo "cd $F && LD_LIBRARY_PATH=$1 ADSP_LIBRARY_PATH='$2;/vendor/lib/rfsa/adsp;/vendor/dsp/cdsp' NPU_BENCH_DISPATCH=$3 ./npu-bench --model-dir $F/m --clips $F/clips 2>&1 | grep -E '^soc=|^clip=ko1|error|denied|fastrpc|Failed' | head -8"
}
case "$1" in
  build)
    w=$(mktemp -d); mkdir -p "$w/lib/arm64-v8a"; cp "$skel" "$stub" "$w/lib/arm64-v8a/"
    [ -f "$HOME/.cache/zyris-gate.jks" ] || nix-shell -p jdk17 --run "keytool -genkeypair -keystore $HOME/.cache/zyris-gate.jks -storepass gategate -keypass gategate -alias gate -dname CN=gate -keyalg RSA -validity 3650" >/dev/null
    $BT/aapt2 link -o "$w/base.apk" --manifest "$here/AndroidManifest.xml" -I "$SDK/platforms/android-36/android.jar"
    (cd "$w" && nix-shell -p zip --run "zip -q -r -0 base.apk lib")
    $BT/zipalign -P 16 -f 4 "$w/base.apk" "$w/aligned.apk"
    nix-shell -p jdk17 --run "$BT/apksigner sign --ks $HOME/.cache/zyris-gate.jks --ks-pass pass:gategate --out $here/npugate.apk $w/aligned.apk"
    rm -rf "$w"; ls -la "$here/npugate.apk" ;;
  install)
    adb install -r "$here/npugate.apk"
    adb shell mkdir -p $T/m/npu
    adb push "$spike/target/aarch64-linux-android/release/npu-bench" "$LITERT_SDK/aar/jni/arm64-v8a/libLiteRt.so" \
      "$ANDROID_NDK/toolchains/llvm/prebuilt/linux-x86_64/sysroot/usr/lib/aarch64-linux-android/libc++_shared.so" \
      "$npu/out/qairt/aarch64-android/libQnnHtp.so" "$npu/out/qairt/aarch64-android/libQnnSystem.so" "$stub" "$skel" \
      "$spike/qairt/v$v/libLiteRtDispatch_Qualcomm.so" $T/ >/dev/null
    adb push "$spike/out/clips" $T/ >/dev/null
    for f in encoder.tflite decoder.tflite generation_config.json tokenizer.json shapes.json; do adb push "$spike/out/whisper-base/$f" $T/m/ >/dev/null; done
    adb push "$spike/out/whisper-base/npu/$soc" $T/m/npu/ >/dev/null
    adb shell chmod -R a+rX $T
    adb shell "run-as $P sh -c 'mkdir -p files && cp -r $T/* files/'"
    adb shell "run-as $P sh -c 'id; cat /proc/self/attr/current; echo; ls files'" ;;
  shell-control)
    adb shell "cd $T && LD_LIBRARY_PATH=$T ADSP_LIBRARY_PATH='$T;/vendor/lib/rfsa/adsp;/vendor/dsp/cdsp' NPU_BENCH_DISPATCH=$T ./npu-bench --model-dir $T/m --clips $T/clips 2>&1 | grep -E '^soc=|^clip=ko1|error' | head -5" ;;
  files-dir)
    adb shell "run-as $P sh -c \"$(bench $F $F $F)\"" ;;
  native-lib-dir)
    N=$(adb shell pm dump $P | grep -m1 legacyNativeLibraryDir= | sed 's/^[^=]*=//' | tr -d '\r')/arm64
    adb shell "run-as $P ls -la $N"
    adb shell "run-as $P sh -c 'cd $F && mkdir -p away && mv -f libQnnHtpV${v}Skel.so libQnnHtpV${v}Stub.so away/ 2>/dev/null; true'"
    adb shell "run-as $P sh -c \"$(bench $F:$N $N $F)\""
    adb shell "run-as $P sh -c 'cd $F && mv -f away/* . 2>/dev/null; true'" ;;
  clean) adb uninstall $P || true; adb shell rm -rf $T ;;
esac
