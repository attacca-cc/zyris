#!/usr/bin/env bash
# MediaTek NeuroPilot's host libraries, where `ai-edge-litert-sdk-mediatek` 2.2.0 looks for them
# (its `data/`). Its setup.py downloads them while building; an install that skipped that step
# leaves the MediaTek AOT plugin unable to load NeuronAdapter. URL and layout from that setup.py.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
sdk=$(ls -d "$here"/.venv/lib/python3*/site-packages/ai_edge_litert_sdk_mediatek)
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
curl -fL -o "$tmp/np.tar.gz" 'https://s3.ap-southeast-1.amazonaws.com/mediatek.neuropilot.com/66f2c33a-2005-4f0b-afef-2053c8654e4f.gz'
mkdir -p "$tmp/x" && tar xzf "$tmp/np.tar.gz" -C "$tmp/x"
rm -rf "$sdk/data" && mkdir -p "$sdk/data" && cp -r "$tmp/x/neuro_pilot/." "$sdk/data/"
ls "$sdk/data"
