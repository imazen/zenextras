#!/usr/bin/env bash
# Differential check of zenwebm against FFmpeg, both directions:
#  1. FFmpeg writes WebM/Matroska (VP9+Opus, AV1+Opus, live mode, VP9 with
#     alpha, H.264+AAC Matroska); zenwebm's packet list must match ffprobe's
#     (track, timestamp, size, keyframe flag). FFmpeg shifts timestamps by
#     the track's CodecDelay; zenwebm reports them as recorded (the decoder
#     adapter applies the delay), so the check applies FFmpeg's shift.
#  2. zenwebm stream-copies each file; ffprobe must read the copy with the
#     same packets, and FFmpeg must decode it to the same frames (framemd5).
# Needs ffmpeg/ffprobe (with libvpx, libaom, libopus; libx264 optional) and
# cargo. Usage: scripts/ffmpeg-oracle.sh [work-dir]
set -euo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
work=${1:-$here/target/ffmpeg-oracle}
mkdir -p "$work"
cd "$work"
ff() { ffmpeg -v error -y "$@"; }
ff -f lavfi -i testsrc=size=96x64:rate=30 -f lavfi -i sine=frequency=440:sample_rate=48000 \
  -t 2 -c:v libvpx-vp9 -g 15 -c:a libopus vp9_opus.webm
ff -f lavfi -i testsrc=size=96x64:rate=25 -f lavfi -i sine=frequency=300:sample_rate=48000 \
  -t 1.5 -c:v libaom-av1 -cpu-used 8 -g 10 -c:a libopus av1_opus.webm
ff -f lavfi -i testsrc=size=64x48:rate=30 -t 1 -c:v libvpx-vp9 -g 10 -live 1 -f webm live_vp9.webm
ff -f lavfi -i testsrc=size=64x48:rate=30 -t 1 -vf format=yuva420p -c:v libvpx-vp9 \
  -auto-alt-ref 0 alpha_vp9.webm
# Not `| grep -q`: under pipefail its early exit fails the pipeline.
encoders=$(ffmpeg -hide_banner -encoders 2>/dev/null)
if grep -q libx264 <<< "$encoders"; then
  ff -f lavfi -i testsrc=size=64x48:rate=24 -f lavfi -i sine=sample_rate=44100 -t 1 \
    -c:v libx264 -c:a aac h264_aac.mkv
fi
(cd "$here" && cargo build -q --release --example webm_remux)
remux=$(cd "$here" && cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')/release/examples/webm_remux
probe() {
  ffprobe -v error -show_entries packet=stream_index,pts,size,flags,duration \
    -show_entries stream=time_base -of csv=p=0 "$1"
}
fail=0
for f in *.webm *.mkv; do
  [ -e "$f" ] || continue
  case $f in *.zen.*) continue ;; esac
  out=${f%.*}.zen.${f##*.}
  "$remux" "$f" "$out" > "$f.zen.csv" 2> "$f.zen.log"
  probe "$f" > "$f.ff.csv" 2>/dev/null || true
  probe "$out" > "$out.ff.csv" 2>/dev/null || true
  python3 - "$f.ff.csv" "$f.zen.csv" "$out.ff.csv" <<'PY' || fail=1
import sys
ff, zen, copy = (open(p).read().split() for p in sys.argv[1:4])
def packets(lines):
    out = []
    for l in lines:
        f = l.split(",")
        if len(f) == 1 or "/" in f[0]:
            continue
        idx, pts, dur, size, flags = f[0], f[1], f[2], f[3], f[4]
        out.append((int(idx), pts, int(size), flags.startswith("K")))
    return out
def ms(pts):  # ffprobe pts is in the stream time base, WebM's is 1/1000
    return None if pts in ("", "N/A") else int(pts)
a = [(i, ms(p), s, k) for i, p, s, k in packets(ff)]
# zenwebm reports the timestamps the file records; FFmpeg shifts each
# track by its CodecDelay (rounded to the nearest tick, ties up). Apply the
# same shift so the comparison checks both readers saw the same bytes.
delay = {}
z = []
for l in zen:
    if l.startswith("#delay,"):
        _, t, ns = l.split(",")
        delay[int(t)] = (int(ns) + 500_000) // 1_000_000
        continue
    t, pts, size, key = l.split(",")
    t = int(t)
    pts = None if pts == "-" else int(pts) // 1_000_000 - delay.get(t, 0)
    z.append((t, pts, int(size), key == "1"))
c = [(i, ms(p), s, k) for i, p, s, k in packets(copy)]
name = sys.argv[1].rsplit(".", 2)[0]
ok = True
if a != z:
    ok = False
    bad = next(i for i, (x, y) in enumerate(zip(a + [None] * 9, z + [None] * 9)) if x != y)
    print(f"{name}: zenwebm != ffprobe at packet {bad}: {a[bad:bad+2]} vs {z[bad:bad+2]}")
if a != c:
    ok = False
    bad = next(i for i, (x, y) in enumerate(zip(a + [None] * 9, c + [None] * 9)) if x != y)
    print(f"{name}: ffprobe of zenwebm copy differs at packet {bad}: {a[bad:bad+2]} vs {c[bad:bad+2]}")
print(f"{name}: {len(a)} packets, demux {'match' if a == z else 'MISMATCH'}, copy {'match' if a == c else 'MISMATCH'}")
sys.exit(0 if ok else 1)
PY
  if ffmpeg -v error -i "$f" -f framemd5 - > "$f.md5" && ffmpeg -v error -i "$out" -f framemd5 - > "$out.md5" \
     && cmp -s <(grep -v '^#' "$f.md5") <(grep -v '^#' "$out.md5"); then
    echo "${f%.*}: decoded frames identical"
  else
    echo "${f%.*}: DECODED FRAMES DIFFER"; fail=1
  fi
done
# 3. Seeking: on each file's first stream (video), seek at or before 25%, 50%
#    and 90% of its packets' times. A random-access seek must land on the
#    last ffprobe key packet at or before the target, an `any` seek on the
#    last packet at or before it, and the next packet must be that one.
(cd "$here" && cargo build -q --release --example webm_seek)
seek=$(dirname "$remux")/webm_seek
for f in *.webm *.mkv; do
  [ -e "$f" ] || continue
  case $f in *.zen.*) continue ;; esac
  ffprobe -v error -select_streams 0 -show_entries packet=pts,flags -of csv=p=0 "$f" > "$f.seek.csv"
  for q in 25 50 90; do
    for scope in key any; do
      got=$("$seek" "$f" 0 "$(python3 -c "
import csv; p=[int(r[0]) for r in csv.reader(open('$f.seek.csv'))]; p.sort(); print(p[len(p)*$q//100])")" \
        $([ $scope = any ] && echo any) 2>&1) || { echo "$f seek $q% $scope: FAILED ($got)"; fail=1; continue; }
      if python3 - "$f" "$q" "$scope" "$got" <<'PY'
import csv, sys
f, q, scope, got = sys.argv[1], int(sys.argv[2]), sys.argv[3], sys.argv[4]
rows = [(int(r[0]), "K" in r[1]) for r in csv.reader(open(f + ".seek.csv"))]
times = sorted(t for t, _ in rows)
target = times[len(times) * q // 100]
want = max(t for t, k in rows if t <= target and (k or scope == "any"))
(landed, _, lk), (nxt, _, nk) = [tuple(int(x) for x in line.split()) for line in got.splitlines()]
ok = landed == want and nxt == landed and (lk == 1 or scope == "any") and lk == nk
print(f"{f}: seek {q}% {scope}: target {target}, landed {landed}, ffprobe says {want}" + ("" if ok else "  MISMATCH"))
sys.exit(0 if ok else 1)
PY
      then :; else fail=1; fi
    done
  done
done
exit $fail
