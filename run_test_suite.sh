#!/bin/bash

lines=$(grep -rn --line-buffered '#\[kani::proof\]' .)

line_array=()

while IFS= read -r line; do 
	line_array+=("$line")
done <<< "$lines"

for input_line in "${line_array[@]}"; do
	file=$(echo "$input_line" | cut -d ':' -f 1)
	line_num=$(echo "$input_line" | cut -d ':' -f 2)
	next_line=$((line_num + 1))
	sed -n "${next_line}p" "$file"
done

