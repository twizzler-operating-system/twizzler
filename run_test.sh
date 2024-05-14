#!/bin/bash

read -r harness

cmd="cargo kani --workspace --exclude monitor unicode-bidi --enable-unstable --ignore-global-asm -Zstubbing --harness $harness"
echo "Running harness $harness with command: $cmd"
timeout 120 $cmd || echo "timeout"

