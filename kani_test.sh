#!/bin/bash
cd kani
cargo build-dev
cd ..
<<<<<<< HEAD
./kani/scripts/cargo-kani --workspace --exclude monitor -Z stubbing -Z unstable-options  --output-into-files
=======
./kani/scripts/cargo-kani --workspace --exclude monitor -Z stubbing  --output-into-files
>>>>>>> 73de36adf36e949d259f1388d0743ca73c227ec3
