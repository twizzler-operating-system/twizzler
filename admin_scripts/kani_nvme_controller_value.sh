#!/bin/bash

# Replace 'input_file.txt' with your actual file name
sed -i '/const _SIZE_CHECKER: \[u8; 0x1000\]/s/0x1000/4112/' ./src/lib/nvme-rs/src/ds/identify/controller.rs
