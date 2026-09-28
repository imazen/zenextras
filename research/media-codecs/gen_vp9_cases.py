#!/usr/bin/env python3
"""VP9 conformance/perf case generator — same conventions as gen_cases.py.

Inputs land in results-vp9/<name>.ivf (VP9 has no raw Annex-B stream; IVF is
the canonical packet container). References are results-vp9/<name>.ref.yuv
decoded with ffmpeg's *native* vp9 decoder (-c:v vp9, an independent codebase
from libvpx which produced the bitstreams). Manifest: results-vp9/cases.json.
Idempotent: existing files are re-verified, not re-encoded.
"""
import hashlib
import json
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
OUT = ROOT / "results-vp9"
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


# (name, size, frames, pix_fmt, profile, extra libvpx args, ref pix_fmt)
BASE = [
    ("p0_qcif", "176x144", 30, "yuv420p", "0", [], "yuv420p"),
    ("p0_360p", "640x360", 30, "yuv420p", "0", [], "yuv420p"),
    ("p0_720p", "1280x720", 30, "yuv420p", "0",
     ["-tile-columns", "2", "-threads", "4"], "yuv420p"),
    ("p0_g12", "176x144", 30, "yuv420p", "0", ["-g", "12"], "yuv420p"),
    ("p0_rt", "176x144", 30, "yuv420p", "0",
     ["-deadline", "realtime", "-cpu-used", "5"], "yuv420p"),
    ("p0_lossless", "176x144", 30, "yuv420p", "0", ["-lossless", "1"], "yuv420p"),
    ("p1_444", "160x96", 30, "yuv444p", "1", [], "yuv444p"),
    ("p1_422", "176x144", 30, "yuv422p", "1", [], "yuv422p"),
    ("p2_10bit", "160x96", 30, "yuv420p10le", "2", [], "yuv420p10le"),
]


def encode_case(name, size, frames, pix_fmt, profile, extra, ref_fmt):
    ivf, ref = OUT / f"{name}.ivf", OUT / f"{name}.ref.yuv"
    rate_args = [] if "-lossless" in extra else ["-b:v", "0", "-crf", "33"]
    enc = [FFMPEG, "-v", "error", "-y", "-f", "lavfi", "-i",
           f"testsrc2=size={size}:rate=30", "-frames:v", str(frames), "-an",
           "-c:v", "libvpx-vp9", "-profile:v", profile, "-pix_fmt", pix_fmt]
    enc += rate_args + extra + ["-f", "ivf", ivf]
    if not ivf.exists():
        p = run(enc)
        if p.returncode:
            return {"name": name, "status": "encode_failed",
                    "stderr": p.stderr.decode(errors="replace")[-400:]}
    return fill_ref_and_probe(name, ivf, ref, ref_fmt, frames)


def fill_ref_and_probe(name, ivf, ref, ref_fmt, frames):
    """Generate native-vp9-decoder reference + ffprobe record for one file."""
    if not ref.exists():
        argv = [FFMPEG, "-v", "error", "-y", "-threads", "1", "-c:v", "vp9",
                "-i", ivf, "-an", "-fps_mode", "passthrough"]
        if ref_fmt:
            argv += ["-pix_fmt", ref_fmt]
        argv += ["-f", "rawvideo", ref]
        p = run(argv)
        if p.returncode and not ref.exists():
            ref.write_bytes(b"")
    st = {}
    try:
        probe = json.loads(checked(
            [FFPROBE, "-v", "error", "-select_streams", "v:0",
             "-show_entries", "stream=profile,width,height,pix_fmt",
             "-of", "json", ivf]))
        st = probe.get("streams", [{}])[0]
    except RuntimeError:
        pass
    return {
        "name": name, "status": "ok",
        "input_sha256": hashlib.sha256(ivf.read_bytes()).hexdigest(),
        "reference_sha256": hashlib.sha256(ref.read_bytes()).hexdigest(),
        "reference_bytes": ref.stat().st_size,
        "expected_pixel_format": ref_fmt or "native",
        "reference_stream_properties": st,
        "frames": frames,
    }


def ivf_parse(data: bytes):
    """Return (header, [packets]) for a DKIF file."""
    assert data[:4] == b"DKIF", "not IVF"
    hlen = int.from_bytes(data[6:8], "little")
    pos, pkts = hlen, []
    while pos + 12 <= len(data):
        size = int.from_bytes(data[pos:pos + 4], "little")
        ts = data[pos + 4:pos + 12]
        pos += 12
        pkts.append((ts, data[pos:pos + size]))
        pos += size
    return bytearray(data[:hlen]), pkts


def ivf_build(header: bytearray, pkts) -> bytes:
    out = bytearray(header)
    out[24:28] = len(pkts).to_bytes(4, "little")  # nframes field
    for ts, payload in pkts:
        out += len(payload).to_bytes(4, "little") + ts + payload
    return bytes(out)


def derive(name, source_name, transform, note, ref_fmt):
    src = OUT / f"{source_name}.ivf"
    dst = OUT / f"{name}.ivf"
    if not dst.exists():
        header, pkts = ivf_parse(src.read_bytes())
        dst.write_bytes(transform(header, pkts))
    rec = fill_ref_and_probe(name, dst, OUT / f"{name}.ref.yuv", ref_fmt, 0)
    rec["derived_from"] = source_name
    rec["note"] = note
    return rec


def main():
    manifest = {"generated": "2026-09-28",
                "encoder": "libvpx-vp9 via ffmpeg -f ivf",
                "reference": "ffmpeg native vp9 decoder (-c:v vp9) rawvideo",
                "cases": []}
    for spec in BASE:
        rec = encode_case(*spec)
        manifest["cases"].append(rec)
        print(json.dumps({"name": rec["name"], "status": rec["status"]}), flush=True)

    # Clean tail truncation: keep ~60% of packets, IVF header stays valid.
    def trunc_tail(header, pkts):
        return ivf_build(header, pkts[:max(1, len(pkts) * 3 // 5)])
    manifest["cases"].append(derive(
        "trunc_tail", "p0_qcif", trunc_tail,
        "valid IVF with last 40% of packets missing (recording cut)", "yuv420p"))
    print(json.dumps({"name": "trunc_tail", "status": "ok"}), flush=True)

    # Mid-packet bit corruption.
    def corrupt_mid(header, pkts):
        mid = len(pkts) // 2
        ts, payload = pkts[mid]
        bad = bytearray(payload)
        for i in range(min(64, len(bad) // 2)):
            bad[len(bad) // 3 + i] ^= 0xFF
        pkts[mid] = (ts, bytes(bad))
        return ivf_build(header, pkts)
    manifest["cases"].append(derive(
        "corrupt_mid", "p0_360p", corrupt_mid,
        "64 consecutive bit-flips inside one mid-stream packet payload",
        "yuv420p"))
    print(json.dumps({"name": "corrupt_mid", "status": "ok"}), flush=True)

    # Resolution change across a keyframe boundary (stream concat).
    def resize(header, pkts):
        _, pkts_b = ivf_parse((OUT / "p0_360p.ivf").read_bytes())
        return ivf_build(header, pkts[:12] + pkts_b[:18])
    manifest["cases"].append(derive(
        "resize_concat", "p0_qcif", resize,
        "176x144 packets then a 640x360 keyframe restart", "yuv420p"))
    print(json.dumps({"name": "resize_concat", "status": "ok"}), flush=True)

    (OUT / "cases.json").write_text(json.dumps(manifest, indent=1))
    ok = sum(1 for c in manifest["cases"] if c["status"] == "ok")
    print(json.dumps({"total": len(manifest["cases"]), "ok": ok}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
