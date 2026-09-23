#!/bin/bash
# Run x.py with the environment `cargo xtask toolchain ports rust` normally sets.
# Must be invoked from the repo root: rt-abi's build.rs locates bindgen via $PWD/toolchain/install/bin.
set -euo pipefail
cd /scratch/dbittman/review/twizzler
export PATH="$(readlink -f toolchain/install/bin):$PATH"
export BOOTSTRAP_SKIP_TARGET_SANITY=1
export TWIZZLER_ABI_SYSROOTS=$(readlink -f toolchain/install/sysroots)
export TWIZZLER_ABI_LLVM_CONFIG=$(readlink -f toolchain/install/bin/llvm-config)
SYSROOT=$(readlink -f toolchain/install/sysroots/x86_64-unknown-twizzler)
export CFLAGS_x86_64_unknown_twizzler="--sysroot=$SYSROOT"
export CXXFLAGS_x86_64_unknown_twizzler="--sysroot=$SYSROOT"
export CC_x86_64_unknown_twizzler=$(readlink -f toolchain/install/bin/clang)
export CXX_x86_64_unknown_twizzler=$(readlink -f toolchain/install/bin/clang++)
export AR_x86_64_unknown_twizzler=$(readlink -f toolchain/install/bin/llvm-ar)
cd toolchain/src/rust
exec ./x.py "$@"
