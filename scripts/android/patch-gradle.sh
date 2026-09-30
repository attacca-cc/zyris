#!/usr/bin/env bash
# Additions to the generated app's build.gradle.kts, once. Shared by CI (mobile.yml) and local builds.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd); app=$root/crates/zyris-app/gen/android/app
grep -q 'zyris: additions' "$app/build.gradle.kts" && exit 0
version=$(grep -A1 '^name = "rustls-platform-verifier-android"$' "$root/Cargo.lock" | sed -n 's/^version = "\(.*\)"$/\1/p')
test -n "$version"
cat >> "$app/build.gradle.kts" <<GRADLE

// zyris: additions (scripts/android/patch-gradle.sh)
repositories {
    maven { url = uri("https://github.com/rustls/rustls-platform-verifier/raw/maven-archive/android-release-support/maven/") }
}
dependencies {
    implementation("org.rustls:rustls-platform-verifier:$version")
    // The ONNX Runtime the speech voice loads, with the QNN EP that reads answers on the NPU.
    // \`ort\` 2.0.0-rc.10 asks for API 22, which 1.24 serves; 1.24 is the first whose QNN EP runs
    // Supertonic's Erf on the HTP. Its own QNN runtime (2.42) is left out: the APK carries
    // QAIRT 2.47 for LiteRT (npu-libs.sh), and there can be one libQnnHtp.so.
    implementation("com.microsoft.onnxruntime:onnxruntime-android-qnn:1.24.3") {
        exclude(group = "com.qualcomm.qti")
    }
}
android {
    buildTypes {
        getByName("release") {
            // R8 removes classes only JNI calls; nothing here is worth the risk of shrinking.
            isMinifyEnabled = false
        }
    }
    // The Hexagon skels must exist as files: the DSP loads them by path (ADSP_LIBRARY_PATH).
    packaging { jniLibs { useLegacyPackaging = true } }
}
GRADLE
mkdir -p "$app/src/main/jniLibs/arm64-v8a"
cp -f "$NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/sysroot/usr/lib/aarch64-linux-android/libc++_shared.so" "$app/src/main/jniLibs/arm64-v8a/"
tail -3 "$app/build.gradle.kts"
