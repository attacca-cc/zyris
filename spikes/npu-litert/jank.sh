#!/usr/bin/env bash
# Scrolls Zyris for 20 s and prints its frame stats. Usage: jank.sh <label>
set -euo pipefail
size=$(adb shell wm size | grep -oE '[0-9]+x[0-9]+' | tail -1); w=${size%x*}; h=${size#*x}
x=$((w / 2)); top=$((h * 3 / 10)); bottom=$((h * 7 / 10))
adb shell dumpsys gfxinfo cc.attacca.zyris reset >/dev/null
for i in $(seq 10); do
  adb shell input swipe $x $bottom $x $top 400; sleep 0.6
  adb shell input swipe $x $top $x $bottom 400; sleep 0.6
done
echo "== $1"; adb shell dumpsys gfxinfo cc.attacca.zyris | grep -E 'Total frames rendered|Janky frames|90th percentile|99th percentile' | head -4
