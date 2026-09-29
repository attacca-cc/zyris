#!/usr/bin/env bash
# The phone build environment: JDK 17 for Gradle, the Android SDK with NDK r27, a rustup of its own
# with the Android target (the system Rust has none), and what whisper.cpp's CMake and bindgen need.
# The SDK is ANDROID_HOME if set, else the nixpkgs androidsdk already in the store (the one the
# 0.2.0 phone builds used). Usage: bash scripts/android/env.sh <command...>
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
SDK=${ANDROID_HOME:-$(ls -d /nix/store/*-androidsdk/libexec/android-sdk 2>/dev/null | head -1)}
[ -d "$SDK/ndk-bundle" ] || { echo "no Android SDK with an NDK; set ANDROID_HOME" >&2; exit 1; }
clang=$(nix-build '<nixpkgs>' --no-out-link -A llvmPackages.libclang.lib)
exec nix-shell -p jdk17 rustup cmake ninja pkg-config openssl perl nodejs_24 pnpm unzip zip gh --run "
  set -euo pipefail
  export RUSTUP_HOME=\$HOME/.cache/zyris-rustup CARGO_HOME=\$HOME/.cache/zyris-cargo
  export PATH=\$CARGO_HOME/bin:$SDK/platform-tools:\$PATH
  rustup default stable >/dev/null 2>&1; rustup target add aarch64-linux-android >/dev/null 2>&1
  export ANDROID_HOME=$SDK ANDROID_SDK_ROOT=$SDK NDK_HOME=$SDK/ndk-bundle ANDROID_NDK=$SDK/ndk-bundle
  export BINDGEN_EXTRA_CLANG_ARGS_aarch64_linux_android=--sysroot=$SDK/ndk-bundle/toolchains/llvm/prebuilt/linux-x86_64/sysroot
  export LIBCLANG_PATH=$clang/lib GGML_NATIVE=OFF ORT_LIB_LOCATION=unused CMAKE_GENERATOR=Ninja
  cd $root && $*
"
