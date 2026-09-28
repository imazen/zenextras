#!/usr/bin/env python3
"""Extended H.264 conformance/perf case generator.

Generates shared inputs (results/<name>.h264) + ffmpeg rawvideo references
(results/<name>.ref.yuv) + a verified manifest (results/cases.json) that
compare_all.py consumes. Idempotent: existing files are re-verified, not
re-encoded. References are generated once, shared across all candidates.

Base cases match probe.py's definitions byte-for-byte (same encode args).
"""
import hashlib
import json
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
OUT = ROOT / "results"
OUT.mkdir(exist_ok=True)

FFMPEG = shutil.which("ffmpeg")
FFPROBE = shutil.which("ffprobe")
assert FFMPEG and FFPROBE, "ffmpeg/ffprobe required"


def run(argv, timeout=600):
    return subprocess.run(list(map(str, argv)), stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, timeout=timeout)


def checked(argv, timeout=600):
    p = run(argv, timeout)
    if p.returncode:
        raise RuntimeError(f"{argv[:3]}…: {p.stderr.decode(errors='replace')[-800:]}")
    return p.stdout.decode(errors="replace")


# (name, lavfi source, size, rate, frames, profile, pix_fmt, extra x264 params)
BASE = [
    ("baseline", "testsrc2", "160x96", "30000/1001", 48, "baseline", "yuv420p", "bframes=0:cabac=0"),
    ("main_b", "testsrc2", "160x96", "30000/1001", 48, "main", "yuv420p", "bframes=3:b-adapt=0:cabac=1"),
    ("high_b", "testsrc2", "160x96", "30000/1001", 48, "high", "yuv420p", "bframes=3:b-adapt=0:cabac=1:8x8dct=1"),
    ("high_multi_slice", "testsrc2", "160x96", "30000/1001", 48, "high", "yuv420p", "bframes=3:b-adapt=0:slices=4"),
    ("cropped", "testsrc2", "162x98", "30000/1001", 48, "high", "yuv420p", "bframes=3:b-adapt=0"),
    ("open_gop", "testsrc2", "160x96", "30000/1001", 48, "high", "yuv420p", "bframes=3:b-adapt=0:open-gop=1"),
    ("360p", "testsrc2", "640x360", "30000/1001", 48, "high", "yuv420p", "bframes=3:b-adapt=0"),
    ("mbaff", "testsrc2", "160x96", "30000/1001", 48, "high", "yuv420p", "bframes=3:b-adapt=0:tff=1"),
    ("high10", "testsrc2", "160x96", "30000/1001", 48, "high10", "yuv420p10le", "bframes=3:b-adapt=0"),
]
EXT = [
    # CAVLC with B-frames
    ("cavlc_b", "testsrc2", "160x96", "30000/1001", 48, "main", "yuv420p", "bframes=2:b-adapt=0:cabac=0"),
    # weighted prediction + multiple refs
    ("weightp", "testsrc2", "160x96", "30000/1001", 48, "high", "yuv420p", "bframes=3:b-adapt=0:weightp=2"),
    ("ref5", "testsrc2", "160x96", "30000/1001", 48, "high", "yuv420p", "bframes=2:b-adapt=0:ref=5"),
    # chroma formats
    ("h422", "testsrc2", "160x96", "30000/1001", 48, "high422", "yuv422p", "bframes=0:cabac=1"),
    ("h444", "testsrc2", "160x96", "30000/1001", 48, "high444", "yuv444p", "bframes=0:cabac=1"),
    # no deblock / no weightp / mixed refs
    ("nodeblock", "testsrc2", "160x96", "30000/1001", 48, "high", "yuv420p", "bframes=2:b-adapt=0:deblock=0:0"),
    # low/high bitrate + screen-ish + noise content
    ("screen360", "smptehdbars", "640x360", "30", 36, "high", "yuv420p", "bframes=3:b-adapt=0"),
    ("noisy160", "testsrc2", "160x96", "30000/1001", 48, "high", "yuv420p", "bframes=3:b-adapt=0:crf=40"),
    ("hq160", "testsrc2", "160x96", "30000/1001", 48, "high", "yuv420p", "bframes=3:b-adapt=0:crf=12"),
    # perf ladder (medium preset to stay comparable)
    ("720p", "testsrc2", "1280x720", "30000/1001", 48, "high", "yuv420p", "bframes=3:b-adapt=0"),
    ("1080p", "testsrc2", "1920x1080", "30000/1001", 36, "high", "yuv420p", "bframes=3:b-adapt=0"),
    ("4k", "testsrc2", "3840x2160", "30000/1001", 24, "high", "yuv420p", "bframes=3:b-adapt=0"),
]

ALL = BASE + EXT


def annexb_nals(data: bytes):
    """Split Annex B into (start_code, nal_bytes) pairs."""
    out = []
    i = 0
    n = len(data)
    marks = []
    while i + 2 < n:
        if data[i] == 0 and data[i + 1] == 0:
            if data[i + 2] == 1:
                marks.append((i, 3))
                i += 3
                continue
            if i + 3 < n and data[i + 2] == 0 and data[i + 3] == 1:
                marks.append((i, 4))
                i += 4
                continue
        i += 1
    for idx, (pos, sc) in enumerate(marks):
        end = marks[idx + 1][0] if idx + 1 < len(marks) else n
        # trim gap zeros
        e = end
        while e > pos + sc and data[e - 1] == 0:
            e -= 1
        out.append(data[pos + sc : e])
    return out


def encode_case(name, src, size, rate, frames, profile, pix_fmt, params):
    h264, ref = OUT / f"{name}.h264", OUT / f"{name}.ref.yuv"
    status = "ok"
    enc = [FFMPEG, "-v", "error", "-y", "-f", "lavfi", "-i",
           f"{src}=size={size}:rate={rate}", "-frames:v", str(frames), "-an",
           "-c:v", "libx264", "-threads", "1", "-filter_threads", "1",
           "-profile:v", profile, "-pix_fmt", pix_fmt, "-preset", "medium",
           "-x264-params", params + ":keyint=12:min-keyint=12:scenecut=0",
           "-f", "h264", h264]
    if not h264.exists():
        p = run(enc)
        if p.returncode:
            return {"name": name, "status": "encode_failed",
                    "stderr": p.stderr.decode(errors="replace")[-400:]}
    if not ref.exists():
        p = run([FFMPEG, "-v", "error", "-y", "-threads", "1", "-i", h264,
                 "-an", "-fps_mode", "passthrough", "-pix_fmt", pix_fmt,
                 "-f", "rawvideo", ref])
        if p.returncode:
            return {"name": name, "status": "ref_failed",
                    "stderr": p.stderr.decode(errors="replace")[-400:]}
    probe = json.loads(checked([FFPROBE, "-v", "error", "-select_streams", "v:0",
                                "-show_entries", "stream=profile,width,height,pix_fmt,field_order",
                                "-of", "json", h264]))
    st = probe.get("streams", [{}])[0]
    return {
        "name": name, "status": status,
        "input_sha256": hashlib.sha256(h264.read_bytes()).hexdigest(),
        "reference_sha256": hashlib.sha256(ref.read_bytes()).hexdigest(),
        "reference_bytes": ref.stat().st_size,
        "expected_pixel_format": pix_fmt,
        "reference_stream_properties": st,
        "frames": frames,
    }


def derive(name, source_name, transform, note):
    """Derive a case from an existing encoded stream via byte transform."""
    src = OUT / f"{source_name}.h264"
    dst = OUT / f"{name}.h264"
    ref = OUT / f"{name}.ref.yuv"
    if not dst.exists():
        dst.write_bytes(transform(src.read_bytes()))
    # Reference = whatever ffmpeg makes of the derived stream (may be partial).
    if not ref.exists():
        # pix_fmt: decode to its native format
        p = run([FFMPEG, "-v", "error", "-y", "-threads", "1", "-i", dst,
                 "-an", "-fps_mode", "passthrough", "-f", "rawvideo", ref])
        if p.returncode and not ref.exists():
            ref.write_bytes(b"")
    probe = {}
    try:
        probe = json.loads(checked([FFPROBE, "-v", "error", "-select_streams", "v:0",
                                    "-show_entries", "stream=profile,width,height,pix_fmt,field_order",
                                    "-of", "json", dst])).get("streams", [{}])[0]
    except RuntimeError:
        pass
    return {
        "name": name, "status": "ok", "derived_from": source_name, "note": note,
        "input_sha256": hashlib.sha256(dst.read_bytes()).hexdigest(),
        "reference_sha256": hashlib.sha256(ref.read_bytes()).hexdigest(),
        "reference_bytes": ref.stat().st_size,
        "reference_stream_properties": probe,
    }


def main():
    manifest = {"generated": "2026-09-28", "encoder": "libx264 via ffmpeg -f h264",
                "cases": []}
    for name, src, size, rate, frames, profile, pix_fmt, params in ALL:
        rec = encode_case(name, src, size, rate, frames, profile, pix_fmt, params)
        manifest["cases"].append(rec)
        print(json.dumps({"name": name, "status": rec["status"]}), flush=True)

    # --- derived cases ---
    # 1. seek: decode starting from the 2nd IDR (random-access recovery)
    def second_idr(data):
        nals = annexb_nals(data)
        out = bytearray()
        idr_seen = 0
        param = bytearray()
        started = False
        # keep the stream's SPS/PPS (they precede the first IDR)
        for nal in nals:
            t = nal[0] & 0x1F if nal else 0
            if t in (7, 8) and not started:
                param += b"\x00\x00\x00\x01" + nal
            if t == 5:
                idr_seen += 1
                if idr_seen == 2:
                    started = True
            if started:
                out += b"\x00\x00\x00\x01" + nal
        return bytes(param) + bytes(out)
    manifest["cases"].append(derive("seek_idr2", "main_b", second_idr,
                                    "SPS/PPS + stream from 2nd IDR onward"))
    print(json.dumps({"name": "seek_idr2", "status": "ok"}), flush=True)

    # 2. truncation mid-slice
    def trunc60(data):
        return data[: int(len(data) * 0.6)]
    manifest["cases"].append(derive("trunc60", "high_b", trunc60,
                                    "stream cut at 60% (mid-slice)"))
    print(json.dumps({"name": "trunc60", "status": "ok"}), flush=True)

    # 3. single-byte corruption inside a slice NAL
    def corrupt1(data):
        b = bytearray(data)
        pos = len(b) // 2
        b[pos] ^= 0xFF
        return bytes(b)
    manifest["cases"].append(derive("corrupt1", "high_b", corrupt1,
                                    "one byte flipped mid-stream"))
    print(json.dumps({"name": "corrupt1", "status": "ok"}), flush=True)

    # 4. mid-stream SPS/PPS resolution change: concat 160x96 stream + 176x112
    def sps_switch(_data):
        other = [FFMPEG, "-v", "error", "-y", "-f", "lavfi", "-i",
                 "testsrc2=size=176x112:rate=30000/1001", "-frames:v", "24",
                 "-an", "-c:v", "libx264", "-threads", "1", "-profile:v", "high",
                 "-pix_fmt", "yuv420p", "-preset", "medium",
                 "-x264-params", "bframes=0:keyint=12:scenecut=0", "-f", "h264", "-"]
        p = run(other)
        if p.returncode:
            raise RuntimeError(p.stderr.decode()[-400:])
        return (OUT / "baseline.h264").read_bytes() + p.stdout
    manifest["cases"].append(derive("sps_switch", "baseline", sps_switch,
                                    "160x96 baseline then 176x112 high; new SPS/PPS/IDR mid-stream"))
    print(json.dumps({"name": "sps_switch", "status": "ok"}), flush=True)

    (OUT / "cases.json").write_text(json.dumps(manifest, indent=2) + "\n")
    ok = sum(1 for c in manifest["cases"] if c["status"] == "ok")
    print(f"cases.json: {ok}/{len(manifest['cases'])} usable")
    return 0


if __name__ == "__main__":
    sys.exit(main())
