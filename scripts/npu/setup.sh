#!/usr/bin/env bash
# The exporter's Python environment, and what the Qualcomm AOT plugin needs that a wheel install
# leaves out: QAIRT's x86 QNN libraries in the SDK package's data/ directory. (Found in the phase 0
# spike, spikes/npu-litert on branch spike/npu-litert.) QAIRT is the version LiteRT 2.2.0 pins.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
nix-shell -p uv --run "cd '$here' && uv venv .venv --python 3.12 -q && uv pip install -q --python .venv/bin/python litert-torch==0.9.4 ai-edge-litert==2.2.0 ai-edge-litert-sdk-qualcomm==2.2.0 transformers==5.17.0 torch==2.13.0 soundfile==0.14.0 huggingface-hub==1.33.0 tokenizers==0.23.2 numpy==2.5.3"
sdk=$(ls -d "$here"/.venv/lib/python3*/site-packages/ai_edge_litert_sdk_qualcomm)
if [ ! -d "$sdk/data/lib/x86_64-linux-clang" ]; then
  tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
  curl -fsSL -o "$tmp/qairt.zip" 'https://softwarecenter.qualcomm.com/api/download/software/sdks/Qualcomm_AI_Runtime_Community/All/2.47.0.260601/v2.47.0.260601.zip'
  # Pinned: these are the binaries the app will carry.
  echo "d3497e110eae82c35a9152a93c0a18bbede402aaf9faa7a97c8079eb0f522b01  $tmp/qairt.zip" | sha256sum -c -
  nix-shell -p unzip --run "cd '$tmp' && unzip -q qairt.zip 'qairt/2.47.0.260601/lib/x86_64-linux-clang/*' 'qairt/2.47.0.260601/lib/aarch64-android/*' 'qairt/2.47.0.260601/lib/hexagon-v*/unsigned/*' 'qairt/2.47.0.260601/*.txt' 'qairt/2.47.0.260601/*.pdf' 'qairt/2.47.0.260601/LICENSE*' || true"
  mkdir -p "$sdk/data/lib" "$here/out/qairt"
  cp -r "$tmp/qairt/2.47.0.260601/lib/x86_64-linux-clang" "$sdk/data/lib/"
  cp -r "$tmp/qairt/2.47.0.260601/lib/aarch64-android" "$tmp"/qairt/2.47.0.260601/lib/hexagon-v* "$here/out/qairt/"
  find "$tmp/qairt/2.47.0.260601" -maxdepth 1 -type f -exec cp {} "$here/out/qairt/" \;
fi
echo "ready: $here/.venv"
