#!/usr/bin/env bash
# LLVM's libc++ and libunwind, which the Qualcomm AOT plugin links and NixOS keeps off the default
# library path (phase 0 findings).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
cxx=$(nix-build '<nixpkgs>' -A llvmPackages.libcxx --no-out-link)
unwind=$(nix-build '<nixpkgs>' -A llvmPackages.libunwind --no-out-link)
cd "$here" && LD_LIBRARY_PATH="$cxx/lib:$unwind/lib" exec .venv/bin/python aot.py "$@"
