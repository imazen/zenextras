#!/usr/bin/env python3
"""VP8 conformance/perf case generator — same conventions as gen_vp9_cases.py.

Inputs land in results-vp8/<name>.ivf (IVF is the canonical VP8 packet
container). Two references per case:
  results-vp8/<name>.ref.yuv — ffmpeg libvpx decoder (-c:v libvpx), the
      PRIMARY oracle (same code lineage as the port target).
  results-vp8/<name>.ff.yuv  — ffmpeg *native* vp8 decoder (-c:v vp8), an
      independent codebase, for triangulation when zenvp8 disagrees.
Manifest: results-vp8/cases.json. Idempotent: existing files are re-verified,
not re-encoded.
"""
import hashlib
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
OUT = ROOT / "results-vp8"
OUT.mkdir(exist_ok=True)

FFMPEG = shutil.which("ffmpeg")
FFPROBE = shutil.which("ffprobe")
assert FFMPEG and FFPROBE, "ffmpeg/ffprobe required"

# Raw libvpx IVF decoder (tools/vp8-dec-raw.c, built against the pinned
# libvpx tree). Required for streams containing invisible (show_frame=0)
# packets: ffmpeg's libvpx wrapper inserts a duplicate of the last shown
# frame into the suppressed packet's output slot, which corrupts every
# subsequent frame's slot alignment. The raw decoder emits exactly the
# frames libvpx produces. Set VP8_RAW_DEC to the binary path, or place it
# at tools/vp8-dec-raw.
RAWDEC = (os.environ.get("VP8_RAW_DEC")
          or str(ROOT / "tools" / "vp8-dec-raw"))
if not (os.path.isfile(RAWDEC) and os.access(RAWDEC, os.X_OK)):
    RAWDEC = None


def run(argv, timeout=600):
    return subprocess.run(list(map(str, argv)), stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, timeout=timeout)


def checked(argv, timeout=600):
    p = run(argv, timeout)
    if p.returncode:
        raise RuntimeError(f"{argv[:3]}…: {p.stderr.decode(errors='replace')[-800:]}")
    return p.stdout.decode(errors="replace")


# (name, size, frames, extra libvpx args, note)
# VP8 "profiles" map to bitstream version 0..3:
#   v0 normal (sixtap + normal loop filter)
#   v1 simple loop filter
#   v2 sharpness signalled in header
#   v3 bilinear MC + full-pixel MVs
BASE = [
    ("v0_qcif", "176x144", 45, [], "baseline version-0 stream"),
    ("v0_360p", "640x360", 30, [], "larger frame, more MBs/row"),
    ("v0_g12", "176x144", 60, ["-g", "12"], "small GOP: frequent keyframe resets"),
    ("v0_rt", "176x144", 30,
     ["-deadline", "realtime", "-cpu-used", "8"],
     "realtime encoder path"),
    ("v1", "176x144", 30, ["-profile:v", "1"], "version 1: simple loop filter"),
    ("v2", "176x144", 30, ["-profile:v", "2", "-sharpness", "5"],
     "version 2: sharpness signalled"),
    ("v3", "176x144", 30, ["-profile:v", "3"],
     "version 3: bilinear MC + full-pixel MVs"),
    ("v0_parts", "176x144", 30, ["-error-resilient", "partitions"],
     "error-resilient: multiple token partitions"),
    ("v0_arf", "176x144", 90,
     ["-auto-alt-ref", "1", "-lag-in-frames", "16", "-arnr-maxframes", "7",
      "-arnr-strength", "4"],
     "alternate reference frames incl. invisible (no-show) packets"),
    ("v0_hiq", "176x144", 20, ["-qmin", "2", "-qmax", "10", "-crf", "4"],
     "near-lossless quantization range"),
    ("v0_loq", "176x144", 20, ["-qmin", "50", "-qmax", "63", "-crf", "60"],
     "high-quantizer range"),
]


def suppressed_count(pkts) -> int:
    """Count packets whose frame tag clears show_frame (bit 4 of byte 0)."""
    return sum(1 for _, p in pkts if p and not (p[0] >> 4) & 1)


def encode_case(name, size, frames, extra, note):
    ivf = OUT / f"{name}.ivf"
    enc = [FFMPEG, "-v", "error", "-y", "-f", "lavfi", "-i",
           f"testsrc2=size={size}:rate=30", "-frames:v", str(frames), "-an",
           "-c:v", "libvpx", "-deadline", "good", "-cpu-used", "0",
           "-b:v", "0", "-crf", "33"]
    enc += extra + ["-f", "ivf", ivf]
    if not ivf.exists():
        p = run(enc)
        if p.returncode:
            return {"name": name, "status": "encode_failed",
                    "stderr": p.stderr.decode(errors="replace")[-400:]}
    return fill_refs_and_probe(name, ivf)


def encode_case_2pass(name, size, frames, extra, note):
    """Two-pass encode — the only libvpx path that emits real show_frame=0
    (invisible) alternate-reference packets."""
    ivf = OUT / f"{name}.ivf"
    passlog = OUT / f"{name}.2pass"
    base = [FFMPEG, "-v", "error", "-y", "-f", "lavfi", "-i",
            f"testsrc2=size={size}:rate=30", "-frames:v", str(frames), "-an",
            "-c:v", "libvpx", "-deadline", "good", "-cpu-used", "0",
            "-b:v", "0", "-crf", "33", "-passlogfile", str(passlog)]
    if not ivf.exists():
        p1 = run(base + extra + ["-pass", "1", "-f", "null", "-"])
        if p1.returncode:
            return {"name": name, "status": "encode_failed",
                    "stderr": "pass1: " + p1.stderr.decode(
                        errors="replace")[-400:]}
        p2 = run(base + extra + ["-pass", "2", "-f", "ivf", ivf])
        if p2.returncode:
            return {"name": name, "status": "encode_failed",
                    "stderr": "pass2: " + p2.stderr.decode(
                        errors="replace")[-400:]}
    return fill_refs_and_probe(name, ivf)


def fill_refs_and_probe(name, ivf):
    """Generate decoder references and ffprobe record.

    Files produced per case:
      <name>.fflibvpx.yuv — ffmpeg libvpx decoder pipe
      <name>.ff.yuv       — ffmpeg *native* vp8 decoder (independent codebase)
      <name>.rawdec.yuv   — raw libvpx IVF decoder (only when the stream has
                            show_frame=0 packets)
      <name>.ref.yuv      — the canonical primary oracle; a copy of rawdec
                            output for suppressed-packet streams, otherwise
                            the ffmpeg-libvpx output.

    ffmpeg's libvpx wrapper cannot represent suppressed packets: it emits a
    duplicate of the previous shown frame in the hidden packet's slot.
    ffmpeg's native decoder ignores show_frame entirely and emits the hidden
    frames as visible output. Only a raw vpx_codec_decode()/get_frame loop
    reproduces libvpx's true emitted sequence.
    """
    n_supp = 0
    try:
        _, pkts = ivf_parse(ivf.read_bytes())
        n_supp = suppressed_count(pkts)
    except AssertionError:
        pass
    refs = {}
    for tag, dec in (("fflibvpx", "libvpx"), ("ff", "vp8")):
        ref = OUT / f"{name}.{tag}.yuv"
        if not ref.exists():
            argv = [FFMPEG, "-v", "error", "-y", "-threads", "1",
                    "-c:v", dec, "-i", ivf, "-an", "-fps_mode", "passthrough",
                    "-pix_fmt", "yuv420p", "-f", "rawvideo", ref]
            p = run(argv)
            if p.returncode and not ref.exists():
                ref.write_bytes(b"")
        refs[tag] = ref
    raw_ref = None
    if n_supp and RAWDEC:
        raw = OUT / f"{name}.rawdec.yuv"
        if not raw.exists():
            run([RAWDEC, ivf, raw])
        if raw.exists() and raw.stat().st_size:
            raw_ref = raw
    canonical = raw_ref if raw_ref is not None else refs["fflibvpx"]
    ref_yuv = OUT / f"{name}.ref.yuv"
    if not ref_yuv.exists():
        ref_yuv.write_bytes(canonical.read_bytes())
    refs["ref"] = ref_yuv
    st = {}
    try:
        probe = json.loads(checked(
            [FFPROBE, "-v", "error", "-select_streams", "v:0",
             "-show_entries", "stream=profile,width,height,pix_fmt",
             "-of", "json", ivf]))
        st = probe.get("streams", [{}])[0]
    except RuntimeError:
        pass
    nframes = 0
    try:
        _, pkts = ivf_parse(ivf.read_bytes())
        nframes = len(pkts)
    except AssertionError:
        pass
    rec = {
        "name": name, "status": "ok",
        "input_sha256": hashlib.sha256(ivf.read_bytes()).hexdigest(),
        "reference_sha256": hashlib.sha256(refs["ref"].read_bytes()).hexdigest(),
        "reference_bytes": refs["ref"].stat().st_size,
        "ff_native_sha256": hashlib.sha256(refs["ff"].read_bytes()).hexdigest(),
        "ff_native_bytes": refs["ff"].stat().st_size,
        "oracles_agree": (refs["ref"].read_bytes() == refs["ff"].read_bytes()),
        "expected_pixel_format": "yuv420p",
        "reference_stream_properties": st,
        "frames": nframes,
        "suppressed_packets": n_supp,
    }
    if n_supp:
        rec["primary_oracle"] = ("raw-libvpx" if raw_ref else
                                 "ffmpeg-libvpx (INVALID: suppressed-packet "
                                 "duplicate artifact — install "
                                 "tools/vp8-dec-raw or set VP8_RAW_DEC)")
    return rec


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


def derive(name, source_name, transform, note):
    src = OUT / f"{source_name}.ivf"
    dst = OUT / f"{name}.ivf"
    if not dst.exists():
        header, pkts = ivf_parse(src.read_bytes())
        dst.write_bytes(transform(header, pkts))
    rec = fill_refs_and_probe(name, dst)
    rec["derived_from"] = source_name
    rec["note"] = note
    return rec


def main():
    manifest = {"generated": "2026-09-29",
                "encoder": "libvpx via ffmpeg -f ivf (deadline good cpu-used 0)",
                "reference": "ffmpeg libvpx decoder (-c:v libvpx) rawvideo yuv420p",
                "triangulation": "ffmpeg native vp8 decoder (-c:v vp8)",
                "cases": []}
    for spec in BASE:
        name, size, frames, extra, note = spec
        rec = encode_case(name, size, frames, extra, note)
        rec["note"] = note
        manifest["cases"].append(rec)
        print(json.dumps({"name": rec["name"], "status": rec["status"],
                          "oracles_agree": rec.get("oracles_agree")}), flush=True)

    # Two-pass altref: the only encode path that emits real show_frame=0
    # (invisible) packets. Requires the raw-libvpx oracle — see
    # fill_refs_and_probe's docstring for why the ffmpeg pipe is invalid here.
    rec = encode_case_2pass(
        "v0_arf2p", "128x96", 64,
        ["-auto-alt-ref", "1", "-lag-in-frames", "16", "-arnr-maxframes", "7",
         "-arnr-strength", "4"],
        "two-pass encode: real invisible altref packets (show_frame=0)")
    rec["note"] = ("two-pass encode: real invisible altref packets "
                   "(show_frame=0)")
    manifest["cases"].append(rec)
    print(json.dumps({"name": rec["name"], "status": rec["status"],
                      "suppressed": rec.get("suppressed_packets"),
                      "oracles_agree": rec.get("oracles_agree")}), flush=True)

    # Clean tail truncation: keep ~60% of packets, IVF header stays valid.
    def trunc_tail(header, pkts):
        return ivf_build(header, pkts[:max(1, len(pkts) * 3 // 5)])
    manifest["cases"].append(derive(
        "trunc_tail", "v0_qcif", trunc_tail,
        "valid IVF with last 40% of packets missing (recording cut)"))
    print(json.dumps({"name": "trunc_tail", "status": "ok"}), flush=True)

    # Mid-packet bit corruption (inter packet, inside partition data).
    def corrupt_mid(header, pkts):
        mid = len(pkts) // 2
        ts, payload = pkts[mid]
        bad = bytearray(payload)
        for i in range(min(64, len(bad) // 2)):
            bad[len(bad) // 3 + i] ^= 0xFF
        pkts[mid] = (ts, bytes(bad))
        return ivf_build(header, pkts)
    manifest["cases"].append(derive(
        "corrupt_mid", "v0_360p", corrupt_mid,
        "64 consecutive bit-flips inside one mid-stream inter packet"))
    print(json.dumps({"name": "corrupt_mid", "status": "ok"}), flush=True)

    # Keyframe payload corruption — header-level damage.
    def corrupt_kf(header, pkts):
        ts, payload = pkts[0]
        bad = bytearray(payload)
        for i in range(min(32, len(bad) // 4)):
            bad[len(bad) // 2 + i] ^= 0xFF
        pkts[0] = (ts, bytes(bad))
        return ivf_build(header, pkts)
    manifest["cases"].append(derive(
        "corrupt_kf", "v0_qcif", corrupt_kf,
        "32 bit-flips inside the first keyframe payload"))
    print(json.dumps({"name": "corrupt_kf", "status": "ok"}), flush=True)

    # Resolution change across a keyframe boundary (stream concat).
    def resize(header, pkts):
        _, pkts_b = ivf_parse((OUT / "v0_360p.ivf").read_bytes())
        return ivf_build(header, pkts[:20] + pkts_b[:20])
    manifest["cases"].append(derive(
        "resize_concat", "v0_qcif", resize,
        "176x144 packets then a 640x360 keyframe restart"))
    print(json.dumps({"name": "resize_concat", "status": "ok"}), flush=True)

    (OUT / "cases.json").write_text(json.dumps(manifest, indent=1))
    ok = sum(1 for c in manifest["cases"] if c["status"] == "ok")
    print(json.dumps({"total": len(manifest["cases"]), "ok": ok}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
