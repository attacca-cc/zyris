#!/usr/bin/env bash
# Build the arm64 APK, sign it with the release key so it installs over the published app (keeping
# its enrolment), and install it on the phone adb sees. Run through env.sh.
# Usage: build-phone.sh [features, default "voice"]
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd); features=${1:-voice}
app=$root/crates/zyris-app/gen/android/app
if [ ! -d "$app" ]; then
  (cd "$root" && pnpm tauri android init --ci && pnpm tauri icon crates/zyris-app/icons/app-mobile.svg)
fi
bash "$root/scripts/android/patch-gradle.sh"
bash "$root/scripts/android/npu-libs.sh" "$app"
(cd "$root" && pnpm tauri android build --apk --target aarch64 --ci --features "$features")
BT=$ANDROID_HOME/build-tools/36.0.0; K=$HOME/.local/share/zyris-release; mkdir -p "$root/out"
unsigned=$(find "$app/build/outputs/apk" -name '*-release-unsigned.apk' | head -1)
$BT/zipalign -P 16 -f 4 "$unsigned" "$root/out/zyris-aligned.apk"
$BT/apksigner sign --ks "$K/android-release.jks" --ks-pass "file:$K/android-keystore-password" --ks-key-alias zyris --out "$root/out/zyris-phone.apk" "$root/out/zyris-aligned.apk"
ls -la "$root/out/zyris-phone.apk"
adb install -r "$root/out/zyris-phone.apk" | tail -1
