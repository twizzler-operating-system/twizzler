#!/bin/bash

# Replace 'input_file.txt' with your actual file name
sed -i '/const _SIZE_CHECKER: \[u8; 4112\]/s/4112/0x1000/' ./src/lib/nvme-rs/src/ds/identify/controller.rs
