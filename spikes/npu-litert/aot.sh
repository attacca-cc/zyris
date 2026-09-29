#!/usr/bin/env bash
# The AOT compile with what its vendor plugins link and NixOS does not put on the default path:
# LLVM's libc++ and libunwind (Qualcomm's plugin). Usage: aot.sh out/<model> [SOC...]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
cxx=$(nix-build '<nixpkgs>' -A llvmPackages.libcxx --no-out-link)
unwind=$(nix-build '<nixpkgs>' -A llvmPackages.libunwind --no-out-link)
LD_LIBRARY_PATH="$cxx/lib:$unwind/lib" exec "$here/.venv/bin/python" "$here/python/aot.py" "$@"
