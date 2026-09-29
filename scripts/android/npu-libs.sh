#!/usr/bin/env bash
# QAIRT 2.47.0.260601's Android and Hexagon libraries, and LiteRT 2.2.0's Qualcomm dispatch library,
# into the generated app. The QAIRT licence allows these only inside the app
# (scripts/npu/LICENSING.md): never committed, never downloaded at run time.
# Usage: npu-libs.sh <generated app dir, e.g. crates/zyris-app/gen/android/app>
set -euo pipefail
app=$1; cache=${NPU_LIBS_CACHE:-$HOME/.cache/zyris-qairt-2.47.0.260601}
jni=$app/src/main/jniLibs/arm64-v8a; notices=$app/src/main/assets/licenses
if [ ! -f "$cache/done" ]; then
  rm -rf "$cache"; mkdir -p "$cache"; tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
  curl -fsSL -o "$tmp/qairt.zip" 'https://softwarecenter.qualcomm.com/api/download/software/sdks/Qualcomm_AI_Runtime_Community/All/2.47.0.260601/v2.47.0.260601.zip'
  echo "d3497e110eae82c35a9152a93c0a18bbede402aaf9faa7a97c8079eb0f522b01  $tmp/qairt.zip" | sha256sum -c -
  (cd "$tmp" && unzip -q qairt.zip 'qairt/2.47.0.260601/lib/aarch64-android/*' 'qairt/2.47.0.260601/lib/hexagon-v*/unsigned/*' 'qairt/2.47.0.260601/QNN_NOTICE.txt' 'qairt/2.47.0.260601/NOTICE.txt')
  q=$tmp/qairt/2.47.0.260601
  cp "$q/lib/aarch64-android/libQnnHtp.so" "$q/lib/aarch64-android/libQnnSystem.so" "$cache/"
  for v in 69 73 75 79 81; do cp "$q/lib/aarch64-android/libQnnHtpV${v}Stub.so" "$q/lib/hexagon-v$v/unsigned/libQnnHtpV${v}Skel.so" "$cache/"; done
  cp "$q/QNN_NOTICE.txt" "$q/NOTICE.txt" "$cache/"
  # LiteRT's dispatch library is byte-identical across its per-generation folders; v73's serves all.
  gh release download v2.2.0 -R google-ai-edge/LiteRT -p litert_npu_runtime_libraries.zip -D "$tmp"
  unzip -q -o "$tmp/litert_npu_runtime_libraries.zip" 'qualcomm_runtime_v73/*' -d "$tmp/lrt"
  cp "$tmp/lrt/qualcomm_runtime_v73/src/main/jni/arm64-v8a/libLiteRtDispatch_Qualcomm.so" "$cache/"
  # libLiteRt.so from LiteRT's Maven AAR, pinned. Only the native library: the AAR's Kotlin API is
  # built with Kotlin 2.3, which the app's Gradle (Kotlin 1.9) cannot compile against, and nothing
  # here calls it.
  curl -fsSL -o "$tmp/litert.aar" 'https://dl.google.com/android/maven2/com/google/ai/edge/litert/litert/2.2.0/litert-2.2.0.aar'
  echo "624518d72f8a249711a19e9901f480e74f823ca7818260a739cb2c023024807c  $tmp/litert.aar" | sha256sum -c -
  (cd "$tmp" && unzip -q -o litert.aar 'jni/arm64-v8a/libLiteRt.so')
  cp "$tmp/jni/arm64-v8a/libLiteRt.so" "$cache/"
  touch "$cache/done"
fi
mkdir -p "$jni" "$notices"
cp -f "$cache"/lib*.so "$jni/"
chmod u+w "$jni"/lib*.so
cp -f "$cache/QNN_NOTICE.txt" "$cache/NOTICE.txt" "$notices/"
echo "npu libraries: $(ls "$jni" | grep -cE 'Qnn|LiteRt')"
