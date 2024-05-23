#!/bin/bash
cd kani
cargo build-dev
cd ..
./kani/scripts/cargo-kani --workspace --exclude monitor -Z stubbing
