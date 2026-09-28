"""Small generated H.264 correctness probe; not the conformance corpus.

Run through run-heavy. No network, no project mutations, no skipped successes.
Downloads are supplied separately with a Git-blob-verified source manifest.
"""
import hashlib
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess
import shutil
import time

ROOT = Path(__file__).resolve().parent
OUT = ROOT / "results"
OUT.mkdir(exist_ok=True)


def run(argv, timeout=90):
    return subprocess.run(list(map(str, argv)), stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, timeout=timeout)


def checked(argv):
    p = run(argv)
    if p.returncode:
        raise RuntimeError(p.stderr.decode(errors="replace"))
    return p.stdout.decode()


manifest = json.loads((ROOT / "rust_h264-source.json").read_text())
for entry in manifest["files"]:
    p = ROOT / "rust_h264" / entry["path"]
    data = p.read_bytes()
    def git_sha(b):
        return hashlib.sha1(b"blob " + str(len(b)).encode() + b"\0" + b).hexdigest()
    # File-transfer framing may have appended one newline; only remove it if
    # the resulting bytes match the upstream Git blob exactly.
    if git_sha(data) != entry["git_blob_sha"] and data.endswith(b"\n"):
        if git_sha(data[:-1]) == entry["git_blob_sha"]:
            data = data[:-1]
            p.write_bytes(data)
    assert git_sha(data) == entry["git_blob_sha"], entry["path"]

rustc = shutil.which("rustc")
if rustc is None:
    raise RuntimeError("rustc not found on PATH")
lib = OUT / "librust_h264.rlib"
binary = OUT / "probe"
build = [rustc, "--edition=2021", "-C", "opt-level=3", "-F", "unsafe-code",
         "--crate-name", "rust_h264", "--crate-type", "rlib",
         ROOT / "rust_h264/src/lib.rs", "-o", lib]
checked(build)
checked([rustc, "--edition=2021", "-C", "opt-level=3", "-F", "unsafe-code",
         ROOT / "probe.rs", "--extern", f"rust_h264={lib}", "-o", binary])

cases = [
    ("baseline", "160x96", "baseline", "bframes=0:cabac=0", "yuv420p"),
    ("main_b", "160x96", "main", "bframes=3:b-adapt=0:cabac=1", "yuv420p"),
    ("high_b", "160x96", "high", "bframes=3:b-adapt=0:cabac=1:8x8dct=1", "yuv420p"),
    ("high_multi_slice", "160x96", "high", "bframes=3:b-adapt=0:slices=4", "yuv420p"),
    ("cropped", "162x98", "high", "bframes=3:b-adapt=0", "yuv420p"),
    ("open_gop", "160x96", "high", "bframes=3:b-adapt=0:open-gop=1", "yuv420p"),
    ("360p", "640x360", "high", "bframes=3:b-adapt=0", "yuv420p"),
    ("mbaff", "160x96", "high", "bframes=3:b-adapt=0:tff=1", "yuv420p"),
    ("high10", "160x96", "high10", "bframes=3:b-adapt=0", "yuv420p10le"),
]
report = {
    "repository": manifest["repository"], "commit": manifest["commit"],
    "rustc": checked([rustc, "-Vv"]), "ffmpeg": checked(["ffmpeg", "-version"]),
    "platform": platform.platform(), "machine": platform.machine(),
    "cpu": Path("/proc/cpuinfo").read_text().split("model name", 1)[-1].splitlines()[0],
    "affinity": sorted(os.sched_getaffinity(0)), "build": list(map(str, build)),
    "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
    "qualification": "generated smoke cases only; not full conformance",
    "timing": "single-thread in-process Annex B parse + decode + drain; file read excluded; first iteration copies raw output and is excluded from timing summary; no CPU pin, no reference speed comparison",
    "cases": [],
}
for name, size, profile, params, pixel_format in cases:
    source, reference, actual = [OUT / (name + ext) for ext in (".h264", ".ref.yuv", ".got.yuv")]
    encode = ["ffmpeg", "-v", "error", "-y", "-f", "lavfi", "-i", f"testsrc2=size={size}:rate=30000/1001",
              "-frames:v", "48", "-an", "-c:v", "libx264", "-threads", "1", "-filter_threads", "1",
              "-profile:v", profile, "-pix_fmt", pixel_format, "-preset", "medium", "-crf", "23",
              "-x264-params", params + ":keyint=12:min-keyint=12:scenecut=0", "-f", "h264", source]
    row = {"name": name, "encode": list(map(str, encode)), "expected_pixel_format": pixel_format}
    try:
        checked(encode)
        checked(["ffmpeg", "-v", "error", "-y", "-threads", "1", "-i", source,
                 "-an", "-fps_mode", "passthrough", "-pix_fmt", pixel_format, "-f", "rawvideo", reference])
        row["input_sha256"] = hashlib.sha256(source.read_bytes()).hexdigest()
        ref = reference.read_bytes()
        row["reference_sha256"] = hashlib.sha256(ref).hexdigest()
        p = run([binary, source, actual, "1"])
        row.update(exit_code=p.returncode, stderr=p.stderr.decode(errors="replace")[-4000:])
        if p.returncode:
            row["status"] = "decode_error"
        else:
            got = actual.read_bytes()
            row.update(reference_bytes=len(ref), actual_bytes=len(got),
                       output=json.loads(p.stdout.decode().splitlines()[0]))
            row["status"] = "exact" if got == ref else "mismatch"
            if got != ref:
                row["differing_bytes"] = sum(a != b for a, b in zip(got, ref)) + abs(len(got) - len(ref))
            else:
                # Time only cases with exact output and complete frame count.
                measurements = [json.loads(line) for line in checked([binary, source, actual, "8"]).splitlines()][1:]
                row["measurements"] = measurements
                row["median_fps"] = statistics.median(m["frames"] * 1e9 / m["ns"] for m in measurements)
                row["median_first_frame_ms"] = statistics.median(m["first_ns"] / 1e6 for m in measurements)
    except (RuntimeError, subprocess.TimeoutExpired) as e:
        row.update(status="harness_error", error=str(e)[-4000:])
    report["cases"].append(row)
    print(json.dumps({k: v for k, v in row.items() if k not in ("encode", "measurements")}), flush=True)
    (OUT / "report.json").write_text(json.dumps(report, indent=2) + "\n")

raise SystemExit(0 if all(r["status"] == "exact" for r in report["cases"]) else 1)
