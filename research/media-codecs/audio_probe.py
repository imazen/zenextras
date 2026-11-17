"""Probe Opus exact presentation duration using Xiph libopus via FFmpeg."""
import hashlib
import json
from pathlib import Path
import subprocess
import shutil
import struct

ROOT = Path(__file__).resolve().parent
OUT = ROOT / "audio-results"
OUT.mkdir(exist_ok=True)


def run(args):
    return subprocess.run(list(map(str, args)), stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, timeout=90)


def checked(args):
    p = run(args)
    if p.returncode:
        raise RuntimeError(p.stderr.decode(errors="replace"))
    return p.stdout.decode()


manifest = json.loads((ROOT / "ruopus-source.json").read_text())
for f in manifest["files"]:
    p = ROOT / "ruopus" / f["path"]
    data = p.read_bytes()
    def git_sha(b):
        return hashlib.sha1(b"blob " + str(len(b)).encode() + b"\0" + b).hexdigest()
    if git_sha(data) != f["git_blob_sha"] and data.endswith(b"\n") and git_sha(data[:-1]) == f["git_blob_sha"]:
        data = data[:-1]
        p.write_bytes(data)
    assert git_sha(data) == f["git_blob_sha"], f["path"]

rustc = shutil.which("rustc")
if rustc is None:
    raise RuntimeError("rustc not found on PATH")
lib = OUT / "libruopus.rlib"
binary = OUT / "audio_probe"
base = [rustc, "--edition=2024", "--cfg", 'feature="std"', "-C", "opt-level=3",
        "--crate-name", "ruopus", "--crate-type", "rlib", ROOT / "ruopus/src/lib.rs", "-o", lib]
forbidden = run(base + ["-F", "unsafe-code"])
(OUT / "forbid-unsafe.log").write_bytes(forbidden.stderr)
checked(base + ["-D", "unsafe-code"])
checked([rustc, "--edition=2024", "-C", "opt-level=3", "-F", "unsafe-code",
         ROOT / "audio_probe.rs", "--extern", f"ruopus={lib}", "-o", binary])
report = {"repository": manifest["repository"], "commit": manifest["commit"],
          "build": list(map(str, base)), "forbid_unsafe_exit_code": forbidden.returncode,
          "features": "std only; spectrograms disabled; upstream SIMD enabled",
          "rustc": checked([rustc, "-Vv"]), "ffmpeg": checked(["ffmpeg", "-version"]),
          "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
          "scope": "presentation sample counts; not signal quality or full Opus conformance", "cases": []}
report["reference_controls"] = []
for samples in (960, 961):
    raw, encoded, decoded = [OUT / ("control-" + str(samples) + suffix)
                             for suffix in (".in.f32le", ".opus", ".out.f32le")]
    raw.write_bytes(b"".join(struct.pack("<f", 0.8 if i + 1 == samples else 0.0)
                            for i in range(samples)))
    checked(["ffmpeg", "-v", "error", "-y", "-f", "f32le", "-ar", "48000", "-ac", "1", "-i", raw,
             "-c:a", "libopus", "-b:a", "64000", encoded])
    checked(["ffmpeg", "-v", "error", "-y", "-c:a", "libopus", "-i", encoded,
             "-c:a", "pcm_f32le", "-f", "f32le", decoded])
    presented = decoded.stat().st_size // 4
    if presented != samples:
        raise RuntimeError(f"independent libopus control failed: {samples} -> {presented}")
    report["reference_controls"].append({"input_samples": samples, "presented_samples": presented})
for channels in (1, 2):
    for bitrate in (24000, 64000):
        for samples in (1, 120, 959, 960, 961, 1920, 1921):
            name = f"n{samples}-ch{channels}-br{bitrate}"
            encoded, pcm = OUT / (name + ".opus"), OUT / (name + ".f32le")
            checked([binary, samples, channels, bitrate, encoded])
            p = run(["ffmpeg", "-v", "error", "-y", "-threads", "1", "-c:a", "libopus", "-i", encoded,
                     "-vn", "-c:a", "pcm_f32le", "-f", "f32le", pcm])
            decoded_samples = pcm.stat().st_size // (4 * channels) if p.returncode == 0 else None
            row = {"input_samples_per_channel": samples, "channels": channels, "bitrate": bitrate,
                   "decoded_samples_per_channel": decoded_samples, "decoder_exit_code": p.returncode,
                   "stderr": p.stderr.decode(errors="replace")[-2000:],
                   "file_sha256": hashlib.sha256(encoded.read_bytes()).hexdigest(),
                   "status": "exact_duration" if decoded_samples == samples else "duration_mismatch"}
            report["cases"].append(row)
            print(json.dumps(row), flush=True)
            (OUT / "report.json").write_text(json.dumps(report, indent=2) + "\n")

raise SystemExit(0 if all(r["status"] == "exact_duration" for r in report["cases"]) else 1)
