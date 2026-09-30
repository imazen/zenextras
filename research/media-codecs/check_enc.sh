#!/usr/bin/env bash
# check_enc.sh — encode a YUV source with zenvp8 + libvpx-raw, then decode the
# zenvp8 stream with three independent decoders (zenvp8, raw libvpx, ffmpeg
# native vp8) and byte-compare all outputs.
#
# usage: check_enc.sh <src.yuv> <w> <h> <qindex> <nframes> <kf_interval> <name>
set -u
cd "$(dirname "$0")"
SRC=$1; W=$2; H=$3; Q=$4; N=$5; KF=$6; NAME=$7
OUT=results-vp8-enc
ENC=./zenvp8/target/release/examples/enc_raw
DMP=./zenvp8/target/release/examples/dump
DEC=./tools/vp8-dec-raw

ivf=$OUT/$NAME.ivf
$ENC "$SRC" "$W" "$H" "$Q" "$N" "$KF" "$ivf" > "$OUT/$NAME.pkts" || { echo "$NAME: ENCODE FAIL"; exit 1; }
$DMP "$ivf" "$OUT/$NAME.zdec.yuv" > /dev/null 2>&1 || { echo "$NAME: zenvp8 DECODE FAIL"; exit 1; }
$DEC "$ivf" "$OUT/$NAME.ldec.yuv" 2>/dev/null || { echo "$NAME: libvpx DECODE FAIL"; exit 1; }
ffmpeg -v error -y -c:v vp8 -i "$ivf" -f rawvideo -pix_fmt yuv420p "$OUT/$NAME.ffdec.yuv" 2>/dev/null \
    || { echo "$NAME: ffmpeg DECODE FAIL"; exit 1; }

ok=1
cmp -s "$OUT/$NAME.zdec.yuv" "$OUT/$NAME.ldec.yuv" || { echo "$NAME: zenvp8-vs-libvpx MISMATCH"; ok=0; }
cmp -s "$OUT/$NAME.ldec.yuv" "$OUT/$NAME.ffdec.yuv" || { echo "$NAME: libvpx-vs-ffmpeg MISMATCH"; ok=0; }
sz=$(stat -c %s "$OUT/$NAME.pkts")
if [ $ok -eq 1 ]; then echo "$NAME: OK 3-way identical, ${sz}B stream"; else exit 1; fi
