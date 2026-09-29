# The cross-compile environment for the on-device bench. Run commands through it:
#   bash android-env.sh <command...>
# A rustup of its own (the system Rust has no Android target), the NDK r27 the 0.2.0 phone builds
# used, and what whisper.cpp's CMake build and bindgen need on the host.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
SDK=${ANDROID_SDK:-$(ls -d /nix/store/*-androidsdk/libexec/android-sdk | head -1)}
exec nix-shell -p rustup cmake ninja pkg-config llvmPackages.libclang openssl perl --run "
  export RUSTUP_HOME=\$HOME/.cache/zyris-rustup CARGO_HOME=\$HOME/.cache/zyris-cargo
  export PATH=\$CARGO_HOME/bin:$SDK/platform-tools:\$PATH
  rustup default stable >/dev/null 2>&1; rustup target add aarch64-linux-android >/dev/null 2>&1
  export ANDROID_NDK=$SDK/ndk-bundle NDK_HOME=$SDK/ndk-bundle
  TC=\$ANDROID_NDK/toolchains/llvm/prebuilt/linux-x86_64
  export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=\$TC/bin/aarch64-linux-android31-clang
  export CC_aarch64_linux_android=\$TC/bin/aarch64-linux-android31-clang CXX_aarch64_linux_android=\$TC/bin/aarch64-linux-android31-clang++
  export AR_aarch64_linux_android=\$TC/bin/llvm-ar
  export BINDGEN_EXTRA_CLANG_ARGS_aarch64_linux_android=--sysroot=\$TC/sysroot
  export LIBCLANG_PATH=/nix/store/972k9fgbydiyaxz2fi7wyx65s7gkf27n-clang-21.1.8-lib/lib
  export GGML_NATIVE=OFF ORT_LIB_LOCATION=unused CMAKE_GENERATOR=Ninja
  export LITERT_SDK=\$HOME/.cache/litert
  cd $here && $*
"
