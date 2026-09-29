#!/usr/bin/env bash
# Qualcomm AI Runtime 2.47.0.260601's Android and Hexagon libraries, one directory per HTP
# generation, beside LiteRT 2.2.0's Qualcomm dispatch library. The version is the one LiteRT 2.2.0
# pins in its fetch_qualcomm_library.sh, so runtime and compiler agree.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
curl -fL -o "$tmp/qairt.zip" 'https://softwarecenter.qualcomm.com/api/download/software/sdks/Qualcomm_AI_Runtime_Community/All/2.47.0.260601/v2.47.0.260601.zip'
(cd "$tmp" && unzip -q qairt.zip '*.so')
src="$tmp/qairt/2.47.0.260601"
# The AOT compiler plugin links the x86 QNN libraries; `ai-edge-litert-sdk-qualcomm` looks for them in
# its own `data/` directory, which a wheel install leaves empty.
sdk=$(ls -d "$here"/.venv/lib/python3*/site-packages/ai_edge_litert_sdk_qualcomm)
mkdir -p "$sdk/data/lib" && cp -r "$src/lib/x86_64-linux-clang" "$sdk/data/lib/"
gh release download v2.2.0 -R google-ai-edge/LiteRT -p litert_npu_runtime_libraries.zip -D "$tmp"
unzip -q "$tmp/litert_npu_runtime_libraries.zip" -d "$tmp/npu"
for v in 69 73 75 79 81; do
  d="$here/qairt/v$v"; rm -rf "$d"; mkdir -p "$d"
  cp "$src/lib/aarch64-android/libQnnHtp.so" "$src/lib/aarch64-android/libQnnSystem.so" \
     "$src/lib/aarch64-android/libQnnHtpV${v}Stub.so" "$src/lib/hexagon-v${v}/unsigned/libQnnHtpV${v}Skel.so" \
     "$tmp/npu/qualcomm_runtime_v$v/src/main/jni/arm64-v8a/libLiteRtDispatch_Qualcomm.so" "$d/"
done
du -sh "$here"/qairt/v*
