#!/bin/bash
cd ~/kani 
cargo build-dev
cd ~/twizzler
../kani/scripts/cargo-kani --workspace --exclude monitor --exclude twizzler-kernel -Z stubbing -Z unstable-options --output-into-files
