#!/bin/bash
# Logs every other process using >15% CPU, once per ~2 s, until the stop file appears.
# Columns: unix_ms pid %cpu command
out=$1; stop=$2
while [ ! -e "$stop" ]; do
  ts=$(( $(date +%s%N) / 1000000 ))
  top -b -n 2 -d 1 -w 200 | awk -v ts="$ts" '/^top -/{blk++} blk==2 && $1 ~ /^[0-9]+$/ && $9+0 > 15 && $12 !~ /^(openexr-bench|exr-decode-ben|top)/ {print ts, $1, $9, $12}' >> "$out"
done
